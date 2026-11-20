#!/usr/bin/env python3
"""Submit one Qoder input event through the installed native hook."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys


if sys.argv[1:] == ["--version"]:
    print("1.1.64")
    sys.exit(0)

parser = argparse.ArgumentParser()
parser.add_argument("--settings", type=Path, required=True)
parser.add_argument("--request-set-id", required=True)
args = parser.parse_args()
groups = json.loads(args.settings.read_text())["hooks"]["UserPromptSubmit"]
payload = json.dumps({
    "hook_event_name": "UserPromptSubmit",
    "cwd": os.getcwd(),
    "session_id": "fixture-session",
    "request_set_id": args.request_set_id,
    "prompt": "check this input",
})
for group in groups:
    for hook in group["hooks"]:
        subprocess.run(
            [hook["command"], *hook.get("args", [])],
            input=payload, text=True, capture_output=True, timeout=10, check=True,
        )
