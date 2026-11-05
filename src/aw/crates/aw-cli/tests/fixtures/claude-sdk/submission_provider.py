"""Inject Provider transport faults; ordinary requests use the real sec-core Provider."""

import json
import os
from pathlib import Path
import subprocess
import sys
import time

raw = sys.stdin.buffer.read()
request = json.loads(raw)
if request["method"] == "invoke":
    text = request["event"]["input"]["text"]
    with Path("submission-calls.jsonl").open("a") as output:
        output.write(json.dumps(request) + "\n")
    if text.startswith("FAULT:exit"):
        raise SystemExit(9)
    if text.startswith("FAULT:invalid"):
        print("invalid-provider-json")
        raise SystemExit(0)
    if text.startswith("FAULT:timeout") or text.startswith("FAULT:cancel"):
        Path("scanner.pid").write_text(str(os.getpid()))
        time.sleep(30)
result = subprocess.run(sys.argv[1:], input=raw, timeout=10)
raise SystemExit(result.returncode)
