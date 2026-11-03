"""A bounded description barrier exposes policy preparation to race tests."""

import json
from pathlib import Path
import sys
import time

directory = Path(sys.argv[1])
request = json.load(sys.stdin)
reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"], "status": "ok"}
if request["method"] == "describe":
    if (directory / "hold").exists():
        (directory / "preparing").touch()
        deadline = time.monotonic() + 5
        while not (directory / "release").exists():
            if time.monotonic() >= deadline:
                sys.exit(90)
            time.sleep(0.005)
    reply["operations"] = [{"name": "observe", "events": ["tool.before"], "effects": ["observe"]}]
elif request["method"] == "invoke":
    reply.update(input_digest=request["input_digest"], effects=[])
json.dump(reply, sys.stdout)
