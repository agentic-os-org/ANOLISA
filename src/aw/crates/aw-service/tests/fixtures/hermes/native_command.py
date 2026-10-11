#!/usr/bin/env python3
"""Harmless callback fixture for the real Hermes shell-hook dispatcher."""

import argparse
import json
import os
from pathlib import Path
import sys


parser = argparse.ArgumentParser()
parser.add_argument("command")
parser.add_argument("--binding")
parser.add_argument("--adapter")
parser.add_argument("--event")
parser.add_argument("--step")
parser.add_argument("--on-error")
parser.add_argument("--literal")
args = parser.parse_args()
payload = json.loads(sys.stdin.read())
with Path(os.environ["AW_HERMES_TRACE"]).open("a") as handle:
    handle.write(json.dumps({"step": args.step, "cwd": os.getcwd(), "payload": payload,
        "profile_sentinel": os.environ.get("AW_HERMES_PROFILE_SENTINEL"),
        "tmpdir": os.environ.get("TMPDIR"), "home": os.environ.get("HOME"),
        "literal": args.literal}) + "\n")
if args.step == "deny":
    print('{"action":"block","message":"native block"}')
    raise SystemExit(2)
if args.step == "ask":
    print('{"action":"approve","message":"native human approval"}')
else:
    print("{}")
