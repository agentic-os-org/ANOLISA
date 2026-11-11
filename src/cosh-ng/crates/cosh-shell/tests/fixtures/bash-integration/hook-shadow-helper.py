#!/usr/bin/python3 -IS
import os
import sys

root = os.environ["COSH_BASH_TEST_WORKDIR"]
data = sys.stdin.buffer.read(8193)
fields = data[:-1].split(b"\0") if data.endswith(b"\0") else []
functions = sorted(name for name in os.environ if name.startswith("BASH_FUNC_"))
valid = (
    sys.argv[1:] == ["shell-integration", "compat"]
    and len(data) <= 8192
    and len(fields) == 6
    and fields[:2] == [b"2", b"external-bash"]
    and fields[2] in [b"registration", b"activation"]
)
reason = "compatible" if valid and not functions else (
    "unknown_exported_function" if functions else "invalid_request"
)
phase = fields[2].decode("ascii", errors="replace") if len(fields) > 2 else "missing"
with open(os.path.join(root, "hook.calls"), "a") as output:
    output.write(phase + ":" + reason + ":" + ",".join(functions) + "\n")
runtime = os.path.join(root, "runtime")
os.makedirs(runtime, mode=0o700, exist_ok=True)
digest = "a" * 64 if reason == "compatible" else ""
reply = ["2", "SHOW" if digest else "DISABLE", reason, digest, digest, "", runtime, ""]
sys.stdout.buffer.write(b"\0".join(field.encode() for field in reply) + b"\0")
