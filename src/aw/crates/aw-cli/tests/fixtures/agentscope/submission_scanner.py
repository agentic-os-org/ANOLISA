#!/usr/bin/env python3
"""Controlled scanner CLI for the real sec-core Provider's existing operations."""
import json
import os
import sys
import time
from pathlib import Path

root = Path(os.environ["AW_TEST_ROOT"])
text = sys.stdin.read()
prompt = "scan-prompt" in sys.argv
context = json.loads(sys.argv[sys.argv.index("--trace-context") + 1])
with (root / "scanner.jsonl").open("a") as output:
    output.write(json.dumps({"text": text, "prompt": prompt, "context": context}) + "\n")
if text == "error":
    sys.exit(1)
if text == "invalid":
    print("not-json")
    sys.exit(0)
if text in {"timeout", "cancel"}:
    (root / "scan-entered").touch()
    time.sleep(30)
risk = text == ("risk" if prompt else "pii")
if prompt:
    reply = {"schema_version": "1.0", "ok": not risk, "verdict": "deny" if risk else "pass",
             "input_truncated": False, "input_bytes_scanned": len(text.strip().encode()),
             "degraded": False, "layers_failed": [], "layer_results": [{"layer": "rule_engine"}]}
else:
    reply = {"ok": True, "verdict": "deny" if risk else "pass", "summary": {
        "source": "user_input", "truncated": False, "bytes_scanned": len(text.encode())}}
print(json.dumps(reply))
