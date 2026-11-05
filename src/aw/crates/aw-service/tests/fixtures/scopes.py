"""A bounded Provider records dispatch and holds the ordered predecessor."""

import json
import os
from pathlib import Path
import sys
import time

directory = Path(sys.argv[1])
request = json.load(sys.stdin)
reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"], "status": "ok"}
if request["method"] == "describe":
    reply["operations"] = [
        {"name": name, "events": ["tool.after"], "effects": ["observe"]}
        for name in ["first", "second"]
    ]
elif request["method"] == "invoke":
    operation = request["operation"]
    (directory / operation).write_text(str(os.getpid()))
    if operation == "first":
        deadline = time.monotonic() + 5
        while not (directory / "release").exists():
            if time.monotonic() >= deadline:
                sys.exit(90)
            time.sleep(0.005)
    reply.update(input_digest=request["input_digest"], effects=[])
json.dump(reply, sys.stdout)
