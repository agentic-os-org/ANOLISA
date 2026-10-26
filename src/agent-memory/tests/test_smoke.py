"""Exercise make smoke against successful and failing MCP subprocesses."""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SERVER = r"""
import json, os, pathlib, sys, time
mode = os.environ.get("FAKE_MODE", "ok")
with open(os.environ["FAKE_RECORD"], "a") as record:
    record.write(json.dumps({"argv": sys.argv[1:], "pid": os.getpid(),
                            "base": os.environ.get("MEMORY_BASE_DIR"),
                            "user": os.environ.get("USER_ID"),
                            "session": os.environ.get("MEMORY_SESSION_DIR"),
                            "inherited_scope": os.environ.get("MCP_CLIENT_NAME"),
                            "inherited_profile": os.environ.get("MEMORY_PROFILE")}) + "\n")
subcommand = sys.argv[-1]
if subcommand in ("init", "info"):
    sys.exit(42 if mode == subcommand else 0)
if mode == "exit42":
    sys.exit(42)
if mode == "empty":
    sys.exit(0)
if mode == "hang":
    time.sleep(60)
if mode == "flood_mid_protocol":
    sys.stdout.write("x" * (1024 * 1024))
    sys.stdout.flush()
    time.sleep(60)
content = ""
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    if mode == "malformed":
        print("not JSON", flush=True)
        continue
    method = request["method"]
    result = {}
    if method == "initialize":
        result = {"protocolVersion": "2024-11-05"}
    elif method == "tools/list":
        result = {"tools": [{"name": name} for name in
                  ("mem_write", "mem_read", "mem_append", "mem_grep", "mem_list")]}
    else:
        name = request["params"]["name"]
        args = request["params"]["arguments"]
        text = "ok"
        error = False
        if name == "mem_write":
            content = args["content"]
        elif name == "mem_append":
            content += args["content"]
        elif name == "mem_read":
            error = args["path"].startswith("../")
            text = "outside" if error else content
            if mode == "traversal" and error:
                try:
                    text = (pathlib.Path(os.environ["MEMORY_BASE_DIR"]) / "user-smoke" / args["path"]).resolve().read_text()
                    error = False
                except FileNotFoundError:
                    error = True
            if mode == "wrong_read" and not error:
                text = "wrong"
        else:
            text = json.dumps([{"path": "notes/smoke.md"}])
        result = {"content": [{"type": "text", "text": text}],
                  "isError": error or mode == "tool_error"}
    response = {"jsonrpc": "2.0", "id": request["id"], "result": result}
    if mode == "rpc_error":
        response = {"jsonrpc": "2.0", "id": request["id"],
                    "error": {"code": -32603, "message": "injected failure"}}
    if mode == "wrong_id":
        response["id"] += 100
    print(json.dumps(response), flush=True)
if mode in ("flood_eof", "flood_eof_hang"):
    sys.stdout.write(("extra output\n" * 100000))
    sys.stdout.flush()
    if mode == "flood_eof_hang":
        time.sleep(60)
sys.exit(42 if mode == "exit_after_response" else 0)
"""


class SmokeTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / "target/release").mkdir(parents=True)
        (self.root / "tests").mkdir()
        shutil.copyfile(ROOT / "Makefile", self.root / "Makefile")
        shutil.copyfile(ROOT / "tests/smoke.py", self.root / "tests/smoke.py")
        (self.root / "Cargo.toml").write_text('[package]\nversion = "0.0.0"\n')
        self.binary = self.root / "target/release/agent-memory"
        self.binary.write_text(f"#!{sys.executable}\n" + SERVER)
        self.binary.chmod(0o755)
        self.record = self.root / "record.jsonl"

    def run_smoke(self, mode, timeout=None):
        command = ["make", "--no-print-directory", "-o", "build", "smoke"]
        if timeout is not None:
            command.append(f"SMOKE_TIMEOUT={timeout}")
        result = subprocess.run(
            command,
            cwd=self.root,
            text=True,
            capture_output=True,
            timeout=10,
            env={
                **os.environ,
                "FAKE_MODE": mode,
                "FAKE_RECORD": str(self.record),
                "MEMORY_BASE_DIR": "/must-not-use-parent-store",
                "USER_ID": "must-not-use-parent-user",
                "MCP_CLIENT_NAME": "must-not-use-parent-scope",
                "MEMORY_PROFILE": "must-not-use-parent-profile",
            },
        )
        records = [json.loads(line) for line in self.record.read_text().splitlines()]
        self.assertTrue(records)
        return result, records

    def assert_isolated_and_cleaned(self, records):
        for record in records:
            self.assertEqual(record["user"], "smoke")
            self.assertIsNone(record["inherited_scope"])
            self.assertIsNone(record["inherited_profile"])
            self.assertNotEqual(record["base"], "/must-not-use-parent-store")
            self.assertFalse(
                Path(record["base"]).parent.exists(), "temporary root or sentinel leaked"
            )
            self.assertFalse(Path(record["session"]).exists(), "temporary sessions leaked")
            self.assertFalse(Path(f"/proc/{record['pid']}").exists(), "child was not reaped")

    def test_success_uses_one_isolated_namespace(self):
        result, records = self.run_smoke("ok")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Smoke test PASSED", result.stdout)
        self.assert_isolated_and_cleaned(records)
        self.assertEqual([r["argv"][-1] for r in records], ["init", "info", "serve"])
        self.assertEqual(len({r["base"] for r in records}), 1)
        self.assertEqual(len({r["session"] for r in records}), 1)

    def test_failures_never_print_passed(self):
        for mode in (
            "init",
            "info",
            "exit42",
            "empty",
            "malformed",
            "rpc_error",
            "tool_error",
            "wrong_read",
            "wrong_id",
            "exit_after_response",
            "traversal",
        ):
            with self.subTest(mode=mode):
                self.record.unlink(missing_ok=True)
                result, records = self.run_smoke(mode)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertNotIn("Smoke test PASSED", result.stdout)
                self.assert_isolated_and_cleaned(records)

    def test_timeout_reaps_server_and_cleans_data(self):
        result, records = self.run_smoke("hang", timeout=0.5)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("timed out", result.stderr)
        self.assert_isolated_and_cleaned(records)

    def test_stdout_flood_is_bounded_and_cleaned(self):
        for mode in ("flood_eof", "flood_eof_hang", "flood_mid_protocol"):
            with self.subTest(mode=mode):
                self.record.unlink(missing_ok=True)
                started = time.monotonic()
                result, records = self.run_smoke(mode, timeout=0.5)
                self.assertLess(time.monotonic() - started, 4)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("Smoke test PASSED", result.stdout)
                self.assertNotIn("Traceback", result.stderr)
                self.assert_isolated_and_cleaned(records)


if __name__ == "__main__":
    unittest.main()
