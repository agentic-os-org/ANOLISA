#!/usr/bin/env python3
"""Locale-independent codecs for the standalone Codex hook scripts.

Each of these scripts talks three ways: UTF-8 JSON in on stdin, UTF-8 JSON
over a pipe to Core, and a JSON envelope out on its own stdout for the host.
Before this contract was pinned, all three directions trusted the caller's
locale:

* stdin was decoded with ``sys.stdin.read()``, so a UTF-8 payload such as a
  Chinese command was dropped under ``LC_ALL=C`` (fails open, no rewrite);
* ``rewrite-hook`` encoded ``ensure_ascii=False`` requests through a
  ``text=True`` pipe, which raised ``UnicodeEncodeError`` (exit 1) when the
  locale could not represent the command;
* ``rewrite-hook``, ``response-diagnostics`` and ``tool-ready`` printed
  ``ensure_ascii=False`` envelopes, so a non-ASCII hint (the diagnostics
  text contains an em dash) crashed the hook instead of passing through.

These tests drive the real scripts under an ASCII locale with a stub
``tokenless`` on ``PATH`` and assert three things together: exit 0, an
ASCII-only stdout, and a payload that still carries the non-ASCII text once
the host decodes it.
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
SCRIPTS = TESTS_DIR.parent / "adapters" / "tokenless" / "codex" / "scripts"

ASCII_LOCALE = {
    "LC_ALL": "C",
    "LANG": "C",
    "PYTHONCOERCECLOCALE": "0",
    "PYTHONUTF8": "0",
}

COMMAND = "帮我看看日志"
REWRITTEN = "/stub/tokenless rtk 帮我看看日志"
MISSING = "依赖补丁"

REWRITE_STUB = f"""#!{sys.executable}
import json, os, sys
# Core speaks UTF-8 bytes regardless of the hook's locale; read the request
# and write the response as such instead of inheriting the ASCII codec.
request = json.loads(sys.stdin.buffer.read().decode("utf-8"))
with open(os.environ["REQUEST_LOG"], "w", encoding="utf-8") as log:
    json.dump(request, log, ensure_ascii=False)
arguments = dict(request["input"]["arguments"])
arguments["command"] = "/stub/tokenless rtk " + arguments["command"]
sys.stdout.buffer.write(json.dumps(
    {{
        "protocol_version": 2,
        "operation": "pre_tool",
        "result": {{
            "arguments": arguments,
            "action": "replace_arguments",
            "output_optimization": "rtk",
        }},
    }},
    ensure_ascii=False,
).encode("utf-8"))
"""

UNDECODABLE_STUB = f"""#!{sys.executable}
import sys
sys.stdout.buffer.write(b"\\xff\\xfe")
"""

PARTIAL_STUB = f"""#!{sys.executable}
import json, sys
if sys.argv[1:2] != ["env-check"]:
    sys.exit(2)
sys.stdout.buffer.write(
    json.dumps({{"status": "PARTIAL", "missing": [{MISSING!r}]}}, ensure_ascii=False).encode("utf-8")
)
"""


@unittest.skipUnless(os.name == "posix", "the ASCII locale fixture is POSIX-only")
class StandaloneCodecTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.home = self.root / "home"
        self.home.mkdir()
        self.request_log = self.root / "request.json"

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def install_stub(self, body: str) -> None:
        path = self.bin / "tokenless"
        path.write_text(body, encoding="utf-8")
        path.chmod(path.stat().st_mode | stat.S_IXUSR)

    def env(self) -> dict[str, str]:
        # Runtime detection and a declared agent id would only change
        # attribution, not the codec under test; strip them so runs stay
        # deterministic on machines that happen to export them.
        env = {
            key: value
            for key, value in os.environ.items()
            if key
            not in (
                "COSH_NG_VERSION",
                "COSH_RUNTIME",
                # PYTHONIOENCODING would override the locale under test;
                # inheriting it from the caller silently makes the fixture
                # vacuous on machines that export it.
                "PYTHONIOENCODING",
                "TOKENLESS_AGENT_ID",
                "TOKENLESS_BIN",
            )
        }
        env.update(ASCII_LOCALE)
        env.update(
            {
                "HOME": str(self.home),
                "PATH": f"{self.bin}:{env.get('PATH', '')}",
                "REQUEST_LOG": str(self.request_log),
            }
        )
        return env

    def run_script(self, name: str, payload: object) -> subprocess.CompletedProcess:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        return subprocess.run(
            [sys.executable, str(SCRIPTS / name)],
            input=body,
            capture_output=True,
            env=self.env(),
            timeout=15,
            check=False,
        )

    def test_rewrite_hook_carries_a_non_ascii_command(self) -> None:
        self.install_stub(REWRITE_STUB)
        proc = self.run_script(
            "rewrite-hook",
            {
                "session_id": "session-1",
                "tool_use_id": "call-1",
                "tool_name": "Bash",
                "tool_input": {"command": COMMAND, "timeout": 30},
            },
        )

        self.assertEqual(proc.returncode, 0, proc.stderr.decode("utf-8", "replace"))
        self.assertTrue(proc.stdout.isascii(), proc.stdout)
        envelope = json.loads(proc.stdout)["hookSpecificOutput"]
        self.assertEqual(
            envelope["updatedInput"], {"command": REWRITTEN, "timeout": 30}
        )
        request = json.loads(self.request_log.read_text(encoding="utf-8"))
        self.assertEqual(request["input"]["arguments"]["command"], COMMAND)

    def test_rewrite_hook_fails_open_when_core_output_is_not_utf8(self) -> None:
        self.install_stub(UNDECODABLE_STUB)
        proc = self.run_script(
            "rewrite-hook",
            {"tool_name": "Bash", "tool_input": {"command": "grep error log"}},
        )

        self.assertEqual(proc.returncode, 0, proc.stderr.decode("utf-8", "replace"))
        self.assertEqual(proc.stdout, b"")
        self.assertNotIn(b"Traceback", proc.stderr)

    def test_tool_ready_reads_a_non_ascii_env_check_reply(self) -> None:
        self.install_stub(PARTIAL_STUB)
        proc = self.run_script("tool-ready", {"tool_name": "Bash"})

        self.assertEqual(proc.returncode, 0, proc.stderr.decode("utf-8", "replace"))
        self.assertTrue(proc.stdout.isascii(), proc.stdout)
        output = json.loads(proc.stdout)["hookSpecificOutput"]
        self.assertEqual(output["permissionDecision"], "allow")
        self.assertIn(MISSING, output["additionalContext"])

    def test_response_diagnostics_escapes_its_hint(self) -> None:
        proc = self.run_script(
            "response-diagnostics",
            {
                "tool_name": "Bash",
                "tool_response": {
                    "stdout": "",
                    "stderr": "bash: frobnicate: command not found",
                    "exit_code": 127,
                },
            },
        )

        self.assertEqual(proc.returncode, 0, proc.stderr.decode("utf-8", "replace"))
        self.assertTrue(proc.stdout.isascii(), proc.stdout)
        context = json.loads(proc.stdout)["hookSpecificOutput"]["additionalContext"]
        self.assertIn("[tokenless:env:ENV_DEPENDENCY_MISSING]", context)
        # The hint is ASCII apart from its em dash, which ASCII-escaped JSON
        # preserves as \u2014 instead of relying on the stdout codec.
        self.assertIn("\u2014", context)


if __name__ == "__main__":
    unittest.main(verbosity=2)
