"""Native Hook failures retain exit codes/signals for bridge error policy checks."""

import json
import os
import signal
import sys
from pathlib import Path

root = Path(sys.argv[1])
event = json.load(sys.stdin)
mode = json.loads(event["tool_call"]["input"])["mode"]
with (root / "native-callbacks.jsonl").open("a") as output:
    output.write(json.dumps({"label": sys.argv[2], "event": event}) + "\n")
if mode == "native-exit":
    sys.exit(1)
if mode == "native-signal":
    os.kill(os.getpid(), signal.SIGTERM)
