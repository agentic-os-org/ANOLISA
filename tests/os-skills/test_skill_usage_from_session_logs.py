#!/usr/bin/env python3
"""Regression tests for skill_usage_from_session_logs.py (#6121, #6126).

Drives the real CLI over a temporary openclaw-format log tree.

Baseline on unchanged main:
- ``test_later_valid_call_is_still_counted`` fails: a ``command`` that is a
  list/number/object reaches ``re.search``, the file-level handler stops the
  session, and the later ``/skills/bar/`` call is silently dropped (foo: 1,
  no bar).
- ``test_total_is_reported_in_date_and_session_modes`` fails because
  ``--mode date`` / ``--mode session`` printed ``Total skill calls: 0`` while
  the report body listed real calls.
"""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "skillfs"
    / "docs"
    / "skills"
    / "skillfs-mount"
    / "scripts"
    / "skill_usage_from_session_logs.py"
)

TS = "2026-01-01T00:00:00Z"


def tool_call(command):
    return json.dumps({
        "timestamp": TS,
        "message": {
            "content": [{"type": "toolCall", "arguments": {"command": command}}]
        },
    })


class SessionLogsTestCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.logs_dir = Path(self.tmp.name)
        sessions = self.logs_dir / "agents" / "agent-1" / "sessions"
        sessions.mkdir(parents=True)
        self.log = sessions / "s1.jsonl"

    def write_log(self, lines):
        self.log.write_text("\n".join(lines) + "\n", encoding="utf-8")

    def run_cli(self, *args):
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--logs-dir", str(self.logs_dir), *args],
            capture_output=True,
            text=True,
            timeout=60,
        )


class TestNonStringCommand(SessionLogsTestCase):
    def test_later_valid_call_is_still_counted(self):
        self.write_log([
            tool_call("python /skills/foo/run.py"),
            tool_call(["bad"]),
            tool_call({"nested": "bad"}),
            tool_call(7),
            tool_call("python /skills/bar/run.py"),
        ])
        proc = self.run_cli("--mode", "summary")
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("foo: 1", proc.stdout)
        self.assertIn("bar: 1", proc.stdout)
        self.assertIn("Total skill calls: 2", proc.stdout)


class TestFooterTotal(SessionLogsTestCase):
    def test_total_is_reported_in_date_and_session_modes(self):
        self.write_log([
            tool_call("python /skills/foo/run.py"),
            tool_call("python /skills/bar/run.py"),
        ])
        for mode in ("date", "session"):
            proc = self.run_cli("--mode", mode)
            self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
            self.assertIn("Total skill calls: 2", proc.stdout, mode)


if __name__ == "__main__":
    unittest.main(verbosity=2, exit=False)
