"""Record subprocess attribution, then replace this process with the real Rust CLI."""

import json
import os
import sys
from pathlib import Path

args = sys.argv[1:]
command = args[2:] if args[:1] == ["--trace-context"] else args
assert command[:1] == ["skill-ledger"], command
with Path(os.environ["SKILLSEC_TEST_CALLS"]).open("a") as stream:
    stream.write(json.dumps({"command": command[1], "pid": os.getpid()}) + "\n")
binary = os.environ["SKILLSEC_TEST_RUST_CLI"]
os.execv(binary, [binary, *args])
