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
- ``test_prefixed_skill_path_is_not_misattributed`` and
  ``test_pipeline_attributes_each_call_to_the_referenced_skill`` fail:
  ``find_skill_in_command`` matched by raw substring, so a call to
  ``web-search`` was attributed to ``web`` whenever the dict iterated the
  shorter path first.
- ``test_cli_attributes_prefixed_skills_end_to_end`` fails on any host whose
  readdir lists the shorter name first (the common case) and pins the fixed
  attribution everywhere.
- ``test_shell_operator_boundaries_still_count`` and
  ``test_cli_counts_subshell_and_redirection_references`` pass on unchanged
  main (the substring match counted these forms); they pin that the boundary
  check keeps counting references that sit directly against a grouping or
  redirection operator.
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


class TestPrefixedSkillAttribution(SessionLogsTestCase):
    """A skill whose path is a prefix of another skill's path must not
    swallow that skill's calls.

    ``find_skill_in_command`` matched ``skill_path in command``, so
    ``/ws/skills/web`` matched inside ``/ws/skills/web-search/run.py`` and
    the call went to whichever skill the dict happened to iterate first.
    """

    def import_script(self):
        sys.path.insert(0, str(SCRIPT.parent))
        import skill_usage_from_session_logs as skill_usage

        return skill_usage

    def release_script(self):
        sys.path.pop(0)
        sys.modules.pop("skill_usage_from_session_logs", None)

    def test_prefixed_skill_path_is_not_misattributed(self):
        skill_usage = self.import_script()
        try:
            paths = {"web": "/ws/skills/web", "web-search": "/ws/skills/web-search"}
            # The shorter skill first: the dict order that misattributed on
            # unchanged main. Dict insertion order is guaranteed, so this
            # red is deterministic.
            self.assertEqual(
                skill_usage.find_skill_in_command(
                    "python /ws/skills/web-search/run.py", paths
                ),
                "web-search",
            )
            # And readdir order must not decide the attribution either way.
            flipped = {
                "web-search": "/ws/skills/web-search",
                "web": "/ws/skills/web",
            }
            self.assertEqual(
                skill_usage.find_skill_in_command(
                    "python /ws/skills/web-search/run.py", flipped
                ),
                "web-search",
            )
        finally:
            self.release_script()

    def test_prefix_skill_still_counts_its_own_references(self):
        skill_usage = self.import_script()
        try:
            paths = {"web": "/ws/skills/web", "web-search": "/ws/skills/web-search"}
            # The boundary check must not over-narrow: every reference shape
            # the substring loop accepted still counts - a file inside, a
            # trailing slash, the command ending on the bare directory, and
            # a shell-quoted directory.
            for command in (
                "python /ws/skills/web/run.py",
                "ls /ws/skills/web/",
                "ls /ws/skills/web",
                'rm -rf "/ws/skills/web"',
            ):
                with self.subTest(command=command):
                    self.assertEqual(
                        skill_usage.find_skill_in_command(command, paths),
                        "web",
                    )
        finally:
            self.release_script()

    def test_pipeline_attributes_each_call_to_the_referenced_skill(self):
        workspace = self.logs_dir / "ws"
        (workspace / "skills" / "web").mkdir(parents=True)
        (workspace / "skills" / "web-search").mkdir(parents=True)
        self.write_log(
            [
                json.dumps({"cwd": str(workspace)}),
                tool_call(f"python {workspace}/skills/web-search/run.py"),
                tool_call(f"python {workspace}/skills/web/run.py"),
            ]
        )
        skill_usage = self.import_script()
        try:
            # The shorter skill first: the readdir order that misattributed
            # on unchanged main. Each call must land on the skill the
            # command actually references.
            ordered = {
                "web": str(workspace / "skills" / "web"),
                "web-search": str(workspace / "skills" / "web-search"),
            }
            with mock.patch.object(
                skill_usage, "get_skill_paths", return_value=ordered
            ):
                _, by_skill, _ = skill_usage.analyze_session_file(self.log)
        finally:
            self.release_script()
        self.assertEqual(by_skill, {"web": 1, "web-search": 1})

    def test_cli_attributes_prefixed_skills_end_to_end(self):
        workspace = self.logs_dir / "ws"
        (workspace / "skills" / "web").mkdir(parents=True)
        (workspace / "skills" / "web-search").mkdir(parents=True)
        self.write_log(
            [
                json.dumps({"cwd": str(workspace)}),
                tool_call(f"python {workspace}/skills/web-search/run.py"),
            ]
        )
        proc = self.run_cli("--mode", "summary")
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("web-search: 1", proc.stdout)
        self.assertNotIn("web: 1", proc.stdout)

    def test_shell_operator_boundaries_still_count(self):
        skill_usage = self.import_script()
        try:
            paths = {"web": "/ws/skills/web", "web-search": "/ws/skills/web-search"}
            # Grouping and redirection operators are shell syntax, not name
            # content: unchanged main counted these references, so the
            # boundary check must keep counting them.
            for command in (
                "(cd /ws/skills/web)",            # subshell, no trailing slash
                "ls /ws/skills/web>listing.txt",  # redirection, no space
                "run</ws/skills/web/input.txt",   # input redirect, no space
                "(/ws/skills/web/run.py)",        # path right after the paren
            ):
                with self.subTest(command=command):
                    self.assertEqual(
                        skill_usage.find_skill_in_command(command, paths),
                        "web",
                    )
        finally:
            self.release_script()

    def test_cli_counts_subshell_and_redirection_references(self):
        workspace = self.logs_dir / "ws"
        (workspace / "skills" / "web").mkdir(parents=True)
        (workspace / "skills" / "web-search").mkdir(parents=True)
        self.write_log(
            [
                json.dumps({"cwd": str(workspace)}),
                tool_call(f"(cd {workspace}/skills/web)"),
                tool_call(f"ls {workspace}/skills/web>listing.txt"),
            ]
        )
        proc = self.run_cli("--mode", "summary")
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("web: 2", proc.stdout)
        self.assertIn("Total skill calls: 2", proc.stdout)
        # Operator-adjacent references belong to the skill actually named,
        # never to its longer neighbour.
        self.assertNotIn("web-search:", proc.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2)
