"""Inject bounded invocation faults while retaining real Tokenless preparation."""
import json
import os
import subprocess
import sys
import time

request = sys.stdin.buffer.read()
mode = os.environ.get("AW_TEST_MODE", "allow")
if json.loads(request)["method"] == "invoke":
    if mode == "after-error":
        raise SystemExit(9)
    if mode == "after-timeout":
        time.sleep(30)
    if mode == "after-invalid":
        print("invalid")
        raise SystemExit(0)
cli = "/bin/false" if mode == "tokenless-error" else sys.argv[2]
result = subprocess.run([sys.argv[1], "--cli", cli, "--data-dir", sys.argv[3]], input=request)
raise SystemExit(result.returncode)
