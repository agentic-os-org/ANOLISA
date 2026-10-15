#!/usr/bin/python3
"""Public CLI fixture: record literal argv, then emit a configured result."""

import json
import os
from pathlib import Path
import sys
import time

root = Path(__file__).resolve().parent
stat = Path(f"/proc/{os.getpid()}/stat").read_text().rsplit(")", 1)[1].split()
(root / "cli.pid").write_text(f"{os.getpid()} {stat[19]}")
(root / "argv.json").write_text(json.dumps(sys.argv[1:]))
case = json.loads((root / "case.json").read_text())
time.sleep(case.get("sleep", 0))
sys.stderr.write(case.get("stderr", "") + "e" * case.get("stderr_repeat", 0))
sys.stderr.flush()
sys.stdout.write(case.get("raw", json.dumps(case.get("stdout"))))
sys.stdout.write("o" * case.get("stdout_repeat", 0))
sys.stdout.flush()
sys.exit(case.get("exit", 0))
