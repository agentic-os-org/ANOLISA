#!/usr/bin/env python3
"""Controlled Ledger outcomes; cryptography remains in the real Ledger suites."""
import json
from pathlib import Path
import sys
import time

assert sys.argv[-3:-1] == ["skill-ledger", "check"], sys.argv
source = Path(sys.argv[-1])
assert source.is_absolute() and (source / "SKILL.md").is_file()
with (source.parent / "ledger.jsonl").open("a") as log:
    log.write(json.dumps({"directory": str(source)}) + "\n")
if source.name == "timeout":
    time.sleep(30)
if source.name == "unavailable":
    sys.exit(3)
status = {"signed": "pass", "unsigned": "none", "backend-denied": "error"}.get(source.name, source.name)
print(json.dumps({"status": status}))
sys.exit(0 if status == "pass" else 1)
