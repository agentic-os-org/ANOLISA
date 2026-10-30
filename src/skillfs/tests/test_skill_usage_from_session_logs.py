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
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parents[1]
    / "docs"
    / "skills"
    / "skillfs-mount"
    / "scripts"
    / "skill_usage_from_session_logs.py"
)

TS = "2026-01-01T00:00:00Z"


def tool_call(command):
    return json.dumps(
        {
            "timestamp": TS,
            "message": {"content": [{"type": "toolCall", "arguments": {"command": command}}]},
        }
    )


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
    def test_skill_path_discovery_failure_does_not_abort(self):
        self.write_log(
            [
                json.dumps({"cwd": "/stale/workspace"}),
                tool_call("python /skills/foo/run.py"),
                tool_call("python /skills/bar/run.py"),
            ]
        )

        sys.path.insert(0, str(SCRIPT.parent))
        try:
            import skill_usage_from_session_logs as skill_usage

            with mock.patch.object(
                skill_usage, "get_skill_paths", side_effect=RuntimeError("stale workspace")
            ):
                workspace, by_skill, _ = skill_usage.analyze_session_file(self.log)
        finally:
            sys.path.pop(0)
            sys.modules.pop("skill_usage_from_session_logs", None)

        self.assertEqual(workspace, "/stale/workspace")
        self.assertEqual(by_skill, {"foo": 1, "bar": 1})

    def test_string_header_workspace_is_retained(self) -> None:
        workspace = self.logs_dir / "workspace with spaces"
        (workspace / "skills" / "foo").mkdir(parents=True)
        self.write_log(
            [
                json.dumps({"cwd": str(workspace)}),
                tool_call(f"python {workspace}/skills/foo/run.py"),
            ]
        )
        proc = self.run_cli("--mode", "summary")
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn(str(workspace), proc.stdout)
        self.assertIn("foo: 1", proc.stdout)
        self.assertIn("Total skill calls: 1", proc.stdout)

    def test_non_string_header_workspace_does_not_abort(self) -> None:
        for cwd in (["bad"], {"path": "bad"}, 7):
            with self.subTest(cwd=cwd):
                self.write_log(
                    [
                        json.dumps({"cwd": cwd}),
                        tool_call("python /skills/foo/run.py"),
                        tool_call("python /skills/bar/run.py"),
                    ]
                )
                proc = self.run_cli("--mode", "summary")
                self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
                self.assertIn("foo: 1", proc.stdout)
                self.assertIn("bar: 1", proc.stdout)
                self.assertIn("Total skill calls: 2", proc.stdout)

    def test_later_valid_call_is_still_counted(self):
        self.write_log(
            [
                tool_call("python /skills/foo/run.py"),
                tool_call(["bad"]),
                tool_call({"nested": "bad"}),
                tool_call(7),
                tool_call("python /skills/bar/run.py"),
            ]
        )
        proc = self.run_cli("--mode", "summary")
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("foo: 1", proc.stdout)
        self.assertIn("bar: 1", proc.stdout)
        self.assertIn("Total skill calls: 2", proc.stdout)

    def test_inaccessible_workspace_does_not_abort(self):
        workspace = self.logs_dir / "workspace"
        skills_dir = workspace / "skills"
        skills_dir.mkdir(parents=True)
        skills_dir.chmod(0)
        self.addCleanup(lambda: skills_dir.chmod(0o755))
        self.write_log(
            [
                json.dumps({"cwd": str(workspace)}),
                tool_call("python /skills/foo/run.py"),
                tool_call("python /skills/bar/run.py"),
            ]
        )

        proc = self.run_cli("--mode", "summary")
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("foo: 1", proc.stdout)
        self.assertIn("bar: 1", proc.stdout)
        self.assertIn("Total skill calls: 2", proc.stdout)


class TestFooterTotal(SessionLogsTestCase):
    def test_total_is_reported_in_date_and_session_modes(self):
        self.write_log(
            [
                tool_call("python /skills/foo/run.py"),
                tool_call("python /skills/bar/run.py"),
            ]
        )
        for mode in ("date", "session"):
            proc = self.run_cli("--mode", mode)
            self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
            self.assertIn("Total skill calls: 2", proc.stdout, mode)


if __name__ == "__main__":
    unittest.main(verbosity=2)
