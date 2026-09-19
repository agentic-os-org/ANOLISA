"""Forward PII to Rust; isolate only the unmigrated observability record sink."""

import json
import os
import sys
from pathlib import Path

args = sys.argv[1:]
command = args[2:] if args[:1] == ["--trace-context"] else args
with Path(os.environ["PII_TEST_CALLS"]).open("a") as stream:
    stream.write(json.dumps({"command": command[0], "pid": os.getpid()}) + "\n")
if command[:1] == ["scan-pii"]:
    binary = os.environ["PII_TEST_RUST_CLI"]
    os.execv(binary, [binary, *args])
if command[:2] == ["observability", "record"]:
    record = json.load(sys.stdin)
    with Path(os.environ["PII_TEST_RECORDS"]).open("a") as stream:
        stream.write(json.dumps(record) + "\n")
    print('{"ok":true}')
else:
    raise SystemExit("unexpected command at PII-only test boundary")
