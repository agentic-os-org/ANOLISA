#!/usr/bin/python3
"""Record scan requests beside the per-consumer executable link."""

import json
import os
from pathlib import Path
import sys
import time

root = Path(__file__).parent
stat = Path(f"/proc/{os.getpid()}/stat").read_text().rsplit(")", 1)[1].split()
(root / "scanner.pid").write_text(f"{os.getpid()} {stat[19]}")
with (root / "scans.jsonl").open("a") as output:
    output.write(json.dumps(sys.argv[1:]) + "\n")
mode = (root / "mode").read_text()
if mode == "timeout":
    time.sleep(30)
if mode == "backend":
    print(json.dumps({"ok": False, "verdict": "error"}))
    sys.exit(1)
print(json.dumps({"ok": True, "verdict": "deny" if mode == "deny" else "pass"}))
