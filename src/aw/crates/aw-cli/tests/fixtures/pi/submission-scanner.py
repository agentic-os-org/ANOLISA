#!/usr/bin/env python3
"""Controlled sec-core CLI boundary for real Provider prompt/PII acceptance."""
import json
import os
from pathlib import Path
import sys
import time

root = Path(os.environ["FAKE_ROOT"])
text = sys.stdin.read()
if 'scan-code' in sys.argv:
    text = sys.argv[sys.argv.index('--code') + 1]
with (root / "scanner.jsonl").open("a") as output:
    output.write(json.dumps({"text": text, "argv": sys.argv[1:], "pid": os.getpid(),
        "start": Path(f"/proc/{os.getpid()}/stat").read_text().rsplit(")", 1)[1].split()[19]}) + "\n")
if text == "error":
    sys.exit(7)
if text == "invalid":
    print("not JSON")
    sys.exit(0)
if text in ("timeout", "cancel"):
    (root / "scan-started").write_text("yes")
    time.sleep(10)
if "scan-code" in sys.argv:
    print(json.dumps({"ok": True, "verdict": "pass"}))
elif "scan-prompt" in sys.argv:
    print(json.dumps({"schema_version": "1.0", "ok": text != "risk",
        "verdict": "deny" if text == "risk" else "pass", "input_truncated": False,
        "input_bytes_scanned": len(text.encode()), "degraded": False, "layers_failed": [],
        "layer_results": [{"layer": "rule_engine"}]}))
elif "scan-pii" in sys.argv:
    print(json.dumps({"ok": True, "verdict": "deny" if text == "api_key=synthetic-secret" else "pass",
        "summary": {"source": "user_input", "truncated": False, "bytes_scanned": len(text.encode())}}))
else:
    raise ValueError("unexpected scanner operation")
