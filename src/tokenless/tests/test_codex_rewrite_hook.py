#!/usr/bin/env python3
"""Fail-open contract tests for the Codex PreToolUse rewrite hook.

The hook's documented contract is fail-open: "all error paths exit 0 with
empty stdout (passthrough)". A payload whose ``tool_input`` decodes to
something other than a JSON object (a list, a number, a plain string, or a
JSON string encoding one of those) has no ``command`` field to rewrite, so it
belongs on that silent path. Upstream, the hook dereferenced the decoded
value with ``.get()`` and crashed with ``AttributeError`` (exit 1), and a
non-string ``command`` only survived through the subprocess call's broad
``except``. These tests pin both nested shapes to the silent pass-through,
and keep the happy path (a dict with a string command) applying the rewrite.

The hook is driven through its real script with an ``rtk`` stub on PATH so
the version gate passes and the payload parsing itself is under test.
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

TESTS_DIR = Path(__file__).resolve().parent
REWRITE_HOOK = (
    TESTS_DIR.parent
    / "adapters"
    / "tokenless"
    / "codex"
    / "scripts"
    / "rewrite-hook"
)

RTK_STUB = (
    "#!/bin/sh\n"
    'if [ "$1" = "--version" ]; then echo \'rtk 0.43.0\'; exit 0; fi\n'
    'if [ "$1" = "rewrite" ]; then echo \'rtk grep --count error log\'; exit 0; fi\n'
    "exit 1\n"
)


class CodexRewriteHookTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        bin_dir = root / "bin"
        bin_dir.mkdir()
        rtk = bin_dir / "rtk"
        rtk.write_text(RTK_STUB)
        rtk.chmod(rtk.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        # Runtime detection and a declared agent id would only change
        # attribution, not the parsing under test; strip them so the runs are
        # deterministic on machines that happen to export them.
        env = {
            key: value
            for key, value in os.environ.items()
            if key not in ("COSH_NG_VERSION", "COSH_RUNTIME", "TOKENLESS_AGENT_ID")
        }
        # HOME is redirected so the hook's session-tracking context file (and
        # any hook_utils state) lands in the sandbox, never in a real HOME.
        self.env = {
            **env,
            "HOME": str(root / "home"),
            "PATH": f"{bin_dir}:{env.get('PATH', '')}",
        }
        (root / "home").mkdir()

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def run_hook(self, payload: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(REWRITE_HOOK)],
            input=payload,
            capture_output=True,
            text=True,
            env=self.env,
            timeout=15,
            check=False,
        )

    def test_non_object_tool_input_passes_through_silently(self) -> None:
        # Every shape decodes to a value without a "command" field: a bare
        # list/int, and JSON strings encoding a list, a number, or plain text.
        payloads = (
            '{"tool_name": "Bash", "tool_input": ["ls"]}',
            '{"tool_name": "Bash", "tool_input": 7}',
            '{"tool_name": "Bash", "tool_input": "[\\"ls\\"]"}',
            '{"tool_name": "Bash", "tool_input": "42"}',
            '{"tool_name": "Bash", "tool_input": "\\"plain text\\""}',
        )
        for payload in payloads:
            with self.subTest(payload=payload):
                proc = self.run_hook(payload)
                self.assertEqual(proc.returncode, 0, msg=f"stderr:\n{proc.stderr}")
                self.assertEqual(proc.stdout, "", msg=f"stdout:\n{proc.stdout}")

    def test_non_string_command_passes_through_silently(self) -> None:
        proc = self.run_hook('{"tool_name": "Bash", "tool_input": {"command": 42}}')
        self.assertEqual(proc.returncode, 0, msg=f"stderr:\n{proc.stderr}")
        self.assertEqual(proc.stdout, "", msg=f"stdout:\n{proc.stdout}")

    def test_object_tool_input_with_command_still_applies_rewrite(self) -> None:
        payload = json.dumps(
            {
                "session_id": "session-1",
                "tool_use_id": "call-1",
                "tool_name": "Bash",
                "tool_input": {"command": "grep error log", "timeout": 30},
            }
        )
        proc = self.run_hook(payload)
        self.assertEqual(proc.returncode, 0, msg=f"stderr:\n{proc.stderr}")
        envelope = json.loads(proc.stdout)["hookSpecificOutput"]
        self.assertEqual(envelope["hookEventName"], "PreToolUse")
        self.assertEqual(envelope["permissionDecision"], "allow")
        self.assertEqual(
            envelope["updatedInput"],
            {"command": "rtk grep --count error log", "timeout": 30},
        )

    def test_json_encoded_object_tool_input_still_applies_rewrite(self) -> None:
        # Codex serializes tool_input as a JSON string in some protocol
        # versions; a JSON string of an OBJECT must keep working.
        payload = json.dumps(
            {
                "tool_name": "Bash",
                "tool_input": json.dumps({"command": "grep error log"}),
            }
        )
        proc = self.run_hook(payload)
        self.assertEqual(proc.returncode, 0, msg=f"stderr:\n{proc.stderr}")
        envelope = json.loads(proc.stdout)["hookSpecificOutput"]
        self.assertEqual(
            envelope["updatedInput"], {"command": "rtk grep --count error log"}
        )

    def test_rtk_no_rewrite_exits_silently(self) -> None:
        # rtk exit 1 = no equivalent rewrite: silent pass-through.
        payload = json.dumps(
            {"tool_name": "Bash", "tool_input": {"command": "grep error log"}}
        )
        stub_root = Path(self.tmp.name)
        rtk = stub_root / "bin" / "rtk"
        rtk.write_text(
            "#!/bin/sh\n"
            'if [ "$1" = "--version" ]; then echo \'rtk 0.43.0\'; exit 0; fi\n'
            "exit 1\n"
        )
        proc = self.run_hook(payload)
        self.assertEqual(proc.returncode, 0, msg=f"stderr:\n{proc.stderr}")
        self.assertEqual(proc.stdout, "", msg=f"stdout:\n{proc.stdout}")


if __name__ == "__main__":
    unittest.main()
