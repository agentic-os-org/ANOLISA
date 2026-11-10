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
        },
        ensure_ascii=False,
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


class TestLogDecoding(SessionLogsTestCase):
    def test_non_utf8_records_are_skipped(self):
        workspace = self.logs_dir / "workspace-é"
        (workspace / "skills" / "web").mkdir(parents=True)
        header = json.dumps({"cwd": str(workspace)}, ensure_ascii=False).encode("utf-8")
        call = tool_call(f"cat {workspace}/skills/web/SKILL.md").encode("utf-8")
        corrupt = call.replace(b"cat", b"cat\xff")
        output = self.logs_dir / "usage.json"
        for bad_line in (b"\xff\xfe", corrupt, b"\xe2\x82"):
            for position in range(3):
                with self.subTest(bad_line=bad_line, position=position):
                    lines = [call, call]
                    lines.insert(position, bad_line)
                    self.log.write_bytes(b"\n".join([header, *lines]))
                    proc = self.run_cli("--mode", "all", "--output", str(output))
                    self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
                    self.assertEqual(proc.stderr, "")
                    self.assertNotIn("Error reading", proc.stdout)
                    self.assertIn("Total skill calls: 2", proc.stdout)
                    report = json.loads(output.read_text(encoding="utf-8"))
                    self.assertEqual(report["workspaces"], {"agent-1": str(workspace)})
                    self.assertEqual(report["by_agent"], {"agent-1": {"web": 2}})
                    self.assertEqual(report["by_date"], {"2026-01-01": {"web": 2}})
                    self.assertEqual(len(report["by_session"]), 1)
                    self.assertEqual(report["by_session"][0]["skills"], {"web": 2})
                    self.assertEqual(report["by_session"][0]["calls"], 2)

    def test_non_utf8_header_keeps_later_calls(self):
        call = tool_call("cat /skills/web/SKILL.md").encode("utf-8")
        self.log.write_bytes(b'{"cwd":"/bad\xff"}\n' + call + b"\n" + call)
        proc = self.run_cli("--mode", "all")
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertNotIn("Error reading", proc.stdout)
        self.assertIn("web: 2", proc.stdout)
        self.assertIn("Total skill calls: 2", proc.stdout)


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

    def test_fallback_ignores_metadata_globs_and_navigation(self):
        skill_usage = self.import_script()
        try:
            # Discovery found nothing, so every one of these goes through the
            # text fallback -- which is where main counted them as skills.
            for command in (
                "cat /ws/skills/skillfs-views.toml",
                "ls /ws/skills/*",
                "ls /ws/skills/..",
                "ls /ws/skills/.",
                "grep -r web /ws/skills/[a-z]*",
            ):
                with self.subTest(command=command):
                    self.assertIsNone(skill_usage.find_skill_in_command(command, None))
                    self.assertIsNone(
                        skill_usage.extract_skill_from_command_direct(command)
                    )
        finally:
            self.release_script()

    def test_fallback_still_counts_a_bare_skill_directory(self):
        skill_usage = self.import_script()
        try:
            # The case this branch exists for: no trailing slash, nothing after
            # the directory name.
            for command in ("ls /ws/skills/web", "cd /ws/skills/web"):
                with self.subTest(command=command):
                    self.assertEqual(
                        skill_usage.find_skill_in_command(command, None), "web"
                    )
                    self.assertEqual(
                        skill_usage.extract_skill_from_command_direct(command), "web"
                    )
        finally:
            self.release_script()

    def test_fallback_keeps_scanning_past_a_rejected_component(self):
        skill_usage = self.import_script()
        try:
            # Rejecting a component must not abandon the rest of the command: the
            # first match here is a glob, the real skill is behind it.
            self.assertEqual(
                skill_usage.find_skill_in_command(
                    "cat /ws/skills/*.toml && ls /ws/skills/web", None
                ),
                "web",
            )
        finally:
            self.release_script()

    def test_cli_does_not_invent_skills_for_inspection_commands(self):
        workspace = self.logs_dir / "ws"
        (workspace / "skills" / "web").mkdir(parents=True)
        (workspace / "skills" / "skillfs-views.toml").write_text("{}", encoding="utf-8")
        self.write_log(
            [
                json.dumps({"cwd": str(workspace)}),
                tool_call(f"cat {workspace}/skills/skillfs-views.toml"),
                tool_call(f"ls {workspace}/skills/*"),
                tool_call(f"ls {workspace}/skills/.."),
                tool_call(f"python {workspace}/skills/web/run.py"),
            ]
        )
        proc = self.run_cli("--mode", "summary")
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("web: 1", proc.stdout)
        self.assertIn("Total skill calls: 1", proc.stdout)
        self.assertNotIn("skillfs-views", proc.stdout)

    def test_fallback_does_not_count_extensionless_files(self):
        workspace = self.logs_dir / "ws"
        (workspace / "skills" / "web").mkdir(parents=True)
        (workspace / "skills" / "LICENSE").write_text("x", encoding="utf-8")
        self.write_log(
            [
                json.dumps({"cwd": str(workspace)}),
                tool_call(f"cat {workspace}/skills/LICENSE"),
                tool_call(f"python {workspace}/skills/web/run.py"),
            ]
        )
        proc = self.run_cli("--mode", "summary")
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("web: 1", proc.stdout)
        self.assertIn("Total skill calls: 1", proc.stdout)
        self.assertNotIn("LICENSE", proc.stdout)

    def test_fallback_keeps_dotted_skill_directories(self):
        # A relative reference never matches the absolute path discovery
        # returns, so it always reaches the fallback -- and a dot is not proof
        # that the component is a file.
        workspace = self.logs_dir / "ws"
        (workspace / "skills" / "web.v2").mkdir(parents=True)
        self.write_log(
            [
                json.dumps({"cwd": str(workspace)}),
                tool_call("python skills/web.v2/run.py"),
            ]
        )
        proc = self.run_cli("--mode", "summary")
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("web.v2: 1", proc.stdout)

        skill_usage = self.import_script()
        try:
            paths = {"web.v2": str(workspace / "skills" / "web.v2")}
            # The absolute form still counts with discovery ...
            self.assertEqual(
                skill_usage.find_skill_in_command(
                    "python /ws/skills/web.v2/run.py", paths
                ),
                "web.v2",
            )
            # ... and with the workspace gone, when discovery returns nothing.
            self.assertEqual(
                skill_usage.extract_skill_from_command_direct(
                    "python /ws/skills/web.v2/run.py"
                ),
                "web.v2",
            )
        finally:
            self.release_script()

    def test_directory_reference_wins_over_a_bare_file_reference(self):
        skill_usage = self.import_script()
        try:
            # Discovery unavailable: the trailing slash is the only signal left,
            # and the base extractor attributed this command to web.
            self.assertEqual(
                skill_usage.find_skill_in_command(
                    "cat /ws/skills/LICENSE && python /ws/skills/web/run.py", None
                ),
                "web",
            )
            # With discovery the bare file is recognised as a file.
            self.assertIsNone(
                skill_usage.find_skill_in_command(
                    "cat /ws/skills/LICENSE", {"web": "/ws/skills/web"}
                )
            )
        finally:
            self.release_script()
    def test_files_only_workspace_does_not_invent_a_skill(self):
        # Discovery answers, and the answer is "no skill directories".  That is
        # not the same as "discovery failed": the degraded shape test must not
        # run, or every ordinary file read under a skills/ tree becomes a call.
        workspace = self.logs_dir / "ws"
        (workspace / "skills").mkdir(parents=True)
        (workspace / "skills" / "LICENSE").write_text("x", encoding="utf-8")
        self.write_log(
            [
                json.dumps({"cwd": str(workspace)}),
                tool_call(f"cat {workspace}/skills/LICENSE"),
            ]
        )
        proc = self.run_cli("--mode", "summary")
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("Total skill calls: 0", proc.stdout)
        self.assertNotIn("LICENSE", proc.stdout)

        skill_usage = self.import_script()
        try:
            # Answered-and-empty rejects the bare name ...
            self.assertIsNone(
                skill_usage.find_skill_in_command("cat /ws/skills/LICENSE", {})
            )
            # ... while an unusable workspace still gets the bare-directory
            # support this branch exists for.
            self.assertEqual(
                skill_usage.find_skill_in_command("ls /ws/skills/web", None), "web"
            )
        finally:
            self.release_script()

    def test_cli_counts_bare_references_when_discovery_is_unavailable(self) -> None:
        workspace = self.logs_dir / "ws"
        commands = (
            f"ls {workspace}/skills/web",
            f"cd {workspace}/skills/web",
            f"(cd {workspace}/skills/web)",
            f"ls {workspace}/skills/web>listing.txt",
        )
        self.write_log([json.dumps({"cwd": str(workspace)}), *(tool_call(cmd) for cmd in commands)])
        output = self.logs_dir / "report.json"
        for state in ("missing workspace", "missing skills", "skills is a file"):
            with self.subTest(state=state):
                if state == "missing skills":
                    workspace.mkdir()
                elif state == "skills is a file":
                    (workspace / "skills").write_text("not a directory", encoding="utf-8")
                proc = self.run_cli("--mode", "all", "--output", str(output))
                self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
                self.assertIn("web: 4", proc.stdout)
                self.assertIn("Total skill calls: 4", proc.stdout)
                report = json.loads(output.read_text(encoding="utf-8"))
                self.assertEqual(report["by_agent"], {"agent-1": {"web": 4}})
                self.assertEqual(report["by_date"], {"2026-01-01": {"web": 4}})
                self.assertEqual(report["by_session"][0]["skills"], {"web": 4})

    def test_discovery_errors_preserve_bare_references(self) -> None:
        workspace = self.logs_dir / "ws"
        (workspace / "skills" / "web").mkdir(parents=True)
        self.write_log(
            [
                json.dumps({"cwd": str(workspace)}),
                tool_call(f"ls {workspace}/skills/web"),
                tool_call(f"cd {workspace}/skills/web"),
                tool_call(f"(cd {workspace}/skills/web)"),
                tool_call(f"ls {workspace}/skills/web>listing.txt"),
            ]
        )
        skill_usage = self.import_script()
        try:
            # Mock the filesystem operation, not discovery's return value, so
            # the real discovery-to-parser contract is exercised even as root.
            for operation in ("iterdir", "is_dir"):
                with self.subTest(operation=operation), mock.patch.object(
                    Path, operation, side_effect=PermissionError("unreadable skills")
                ):
                    self.assertIsNone(skill_usage.get_skill_paths(str(workspace)))
                    found_workspace, by_skill, by_date = skill_usage.analyze_session_file(self.log)
                    self.assertEqual(found_workspace, str(workspace))
                    self.assertEqual(by_skill, {"web": 4})
                    self.assertEqual(by_date, {"2026-01-01": {"web": 4}})
        finally:
            self.release_script()

    def test_readable_empty_skills_directory_rejects_bare_references(self) -> None:
        workspace = self.logs_dir / "ws"
        (workspace / "skills").mkdir(parents=True)
        self.write_log(
            [
                json.dumps({"cwd": str(workspace)}),
                tool_call(f"ls {workspace}/skills/web"),
            ]
        )
        skill_usage = self.import_script()
        try:
            self.assertEqual(skill_usage.get_skill_paths(str(workspace)), {})
        finally:
            self.release_script()
        proc = self.run_cli("--mode", "summary")
        self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
        self.assertIn("Total skill calls: 0", proc.stdout)
        self.assertNotIn("web:", proc.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2)
