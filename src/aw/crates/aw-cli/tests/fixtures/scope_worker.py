"""A distinct worker execs each private CLI action with its actual parent PID."""

import json
import os
import subprocess
import sys
from typing import Any

config = json.load(sys.stdin)


def cli(action: str, value: Any) -> subprocess.CompletedProcess[str]:
    argv = [config["aw"], "scope", "--adapter", config["adapter"],
            "--binding", config["binding"], "--action", action]
    if action == "invoke":
        argv += ["--event", "tool.before", "--step", "check", "--on-error", "report"]
    return subprocess.run(argv, input=json.dumps(value), text=True, capture_output=True, timeout=8)


opened = cli("open", {"pid": os.getpid(), "cwd": config["cwd"],
                      "environment": {"AW_SERVICE_MARKER": "child-worker"}})
result = {"pid": os.getpid(), "open": opened.returncode, "error": opened.stderr}
if opened.returncode == 0:
    scope = json.loads(opened.stdout)
    try:
        result["acknowledge"] = cli("acknowledge", scope).returncode
        result["invoke"] = cli("invoke", {"scope": scope, "native_input": json.dumps(config["native"])}).returncode
    finally:
        result["close"] = cli("close", scope).returncode
print(json.dumps(result))
