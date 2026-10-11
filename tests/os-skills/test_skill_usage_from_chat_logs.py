#!/usr/bin/env python3
"""Regression tests for skill_usage_from_chat_logs.py (#6116).

Drives the real CLI over a temporary copilot-shell-format log tree. A log
that interleaves well-formed tool calls with malformed records must still
exit 0 and tally only the well-formed calls.

Baseline on unchanged main: 2 of the 3 tests fail — a non-object line raises
``AttributeError: 'list' object has no attribute 'get'`` and an unhashable
``name`` raises ``TypeError: unhashable type: 'list'``; both abort the run
(exit 1) so the later well-formed call is never counted.
``test_empty_function_call_is_not_an_unnamed_tool`` guards the pre-existing
"falsy functionCall is skipped" behaviour that a naive ``isinstance`` check
would lose.
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
    / "skill_usage_from_chat_logs.py"
)

TS = "2026-01-01T00:00:00Z"


def event(parts):
    return json.dumps({"timestamp": TS, "message": {"parts": parts}})


def skill_call(name):
    return {"functionCall": {"name": "skill", "args": {"skill": name}}}


class ChatLogsTestCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.logs_dir = Path(self.tmp.name)
        chats = self.logs_dir / "projects" / "p1" / "chats"
        chats.mkdir(parents=True)
        self.log = chats / "s1.jsonl"

    def write_log(self, lines):
        self.log.write_text("\n".join(lines) + "\n", encoding="utf-8")

    def run_cli(self, *args):
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--logs-dir", str(self.logs_dir), *args],
            capture_output=True,
            text=True,
            timeout=60,
        )


class TestMalformedEntries(ChatLogsTestCase):
    def test_unhashable_name_and_skill_do_not_abort(self):
        self.write_log([
            event([skill_call("good")]),
            # Valid JSON, but the functionCall values are unhashable.
            event([{"functionCall": {"name": []}}]),
            event([{"functionCall": {"name": "skill", "args": {"skill": ["bad"]}}}]),
            event([skill_call("after")]),
        ])
        proc = self.run_cli("--all-tools")
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("Total skill invocations: 2", proc.stdout)
        self.assertIn("good: 1", proc.stdout)
        self.assertIn("after: 1", proc.stdout)
        self.assertNotIn("bad:", proc.stdout)

    def test_empty_function_call_is_not_an_unnamed_tool(self):
        self.write_log([
            event([skill_call("good")]),
            event([{"functionCall": {}}]),
            event([skill_call("after")]),
        ])
        proc = self.run_cli("--all-tools")
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("2 tool calls total", proc.stdout)
        self.assertIn("skill: 2", proc.stdout)
        self.assertIn("Total skill invocations: 2", proc.stdout)

    def test_non_object_records_are_skipped(self):
        self.write_log([
            "[1, 2]",
            '"a bare string"',
            json.dumps({"message": "user said hello"}),
            json.dumps({"message": {"parts": "not-a-list"}}),
            event([skill_call("after")]),
        ])
        proc = self.run_cli("--all-tools")
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("1 tool calls total", proc.stdout)
        self.assertIn("after: 1", proc.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2, exit=False)
