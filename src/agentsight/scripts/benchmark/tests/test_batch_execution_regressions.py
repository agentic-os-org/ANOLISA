"""One failed analyzer invocation must not hide the remaining sessions."""

import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from test_batch_analyze import batch_analyze


class BatchExecutionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.sessions = [self.root / "first.jsonl", self.root / "second.jsonl"]
        for index, session in enumerate(self.sessions):
            session.write_text(
                json.dumps(
                    {
                        "type": "user",
                        "message": {"role": "user", "content": "Inspect the project. " * 100},
                    }
                )
                + "\n",
                encoding="utf-8",
            )
            os.utime(session, (200 - index, 200 - index))

    def run_batch(self, outcomes: list) -> tuple[str, str]:
        temporary_paths = []
        remaining = iter(outcomes)

        def analyze(arguments: list[str], **kwargs: object) -> subprocess.CompletedProcess:
            temporary_paths.append(Path(arguments[1]))
            outcome = next(remaining)
            if isinstance(outcome, Exception):
                raise outcome
            return outcome

        arguments = [
            "batch-analyze.py",
            "--dir",
            str(self.root),
            "--dim",
            "perf",
            "--min-steps",
            "1",
            "--binary",
            "fixture-analyzer",
        ]
        output, errors = io.StringIO(), io.StringIO()
        with patch.object(sys, "argv", arguments), patch.object(
            batch_analyze, "find_conversation_files", return_value=self.sessions
        ), patch.object(batch_analyze.subprocess, "run", side_effect=analyze):
            with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
                batch_analyze.main()
        self.assertEqual(len(temporary_paths), 2)
        self.assertTrue(all(not path.exists() for path in temporary_paths))
        return output.getvalue(), errors.getvalue()

    def assert_first_failed(self, outcome: object) -> None:
        output, errors = self.run_batch(
            [outcome, subprocess.CompletedProcess([], 0, stdout='{"accepted":true}', stderr="")]
        )
        self.assertEqual(json.loads(output)["_session_id"], "second")
        self.assertIn("Done: 1 analyzed, 0 skipped, 1 failed", errors)
        self.assertEqual(errors.count("  OK (perf)"), 1)

    def test_spawn_errors_count_as_failures_and_continue(self) -> None:
        for error in (
            OSError("spawn failed"),
            FileNotFoundError("fixture missing"),
            PermissionError("not executable"),
        ):
            with self.subTest(error=type(error).__name__):
                self.assert_first_failed(error)

    def test_non_object_json_counts_as_failure_and_continues(self) -> None:
        for value in ([], None, "unexpected", 42, True):
            with self.subTest(value=value):
                self.assert_first_failed(
                    subprocess.CompletedProcess([], 0, stdout=json.dumps(value), stderr="")
                )

    def test_invalid_json_keeps_the_existing_continue_behavior(self) -> None:
        self.assert_first_failed(subprocess.CompletedProcess([], 0, stdout="{", stderr=""))

    def test_timeout_keeps_the_existing_continue_behavior(self) -> None:
        self.assert_first_failed(subprocess.TimeoutExpired("fixture", 300))

    def test_nonzero_exit_keeps_the_existing_continue_behavior(self) -> None:
        self.assert_first_failed(
            subprocess.CompletedProcess([], 1, stdout="", stderr="fixture failed")
        )

    def test_two_successes_keep_metadata_and_summary(self) -> None:
        output, errors = self.run_batch(
            [subprocess.CompletedProcess([], 0, stdout="{}", stderr="") for _ in range(2)]
        )
        decoder = json.JSONDecoder()
        first, end = decoder.raw_decode(output)
        second = json.loads(output[end:])
        self.assertEqual([first["_session_id"], second["_session_id"]], ["first", "second"])
        self.assertEqual(first["_source_file"], str(self.sessions[0]))
        self.assertEqual(first["_steps"], 1)
        self.assertIn("Done: 2 analyzed, 0 skipped, 0 failed", errors)

    @unittest.skipUnless(os.name == "posix", "executable analyzer fixtures require POSIX")
    def test_real_cli_process_continues_after_non_object_output(self) -> None:
        analyzer = self.root / "analyzer.py"
        analyzer.write_text(
            f"#!{sys.executable}\nimport json,sys\nfrom pathlib import Path\n"
            "doc=json.loads(Path(sys.argv[1]).read_text(encoding='utf-8'))\n"
            "print('[]' if doc['session_id']=='first' else '{\"accepted\":true}')\n",
            encoding="utf-8",
        )
        analyzer.chmod(0o700)
        result = self.run_cli(str(analyzer))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["_session_id"], "second")
        self.assertIn("Done: 1 analyzed, 0 skipped, 1 failed", result.stderr)
        self.assertFalse(list(self.root.glob("atif-*.json")))

    def test_real_cli_missing_binary_counts_each_file_without_traceback(self) -> None:
        result = self.run_cli(str(self.root / "missing-analyzer"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertIn("Done: 0 analyzed, 0 skipped, 2 failed", result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        self.assertFalse(list(self.root.glob("atif-*.json")))

    def run_cli(self, binary: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                batch_analyze.__file__,
                "--dir",
                str(self.root),
                "--dim",
                "perf",
                "--min-steps",
                "1",
                "--binary",
                binary,
            ],
            env={
                **os.environ,
                "TMPDIR": str(self.root),
                "TEMP": str(self.root),
                "TMP": str(self.root),
            },
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=20,
        )


class BatchDiscoveryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory(prefix="batch-discovery-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    @staticmethod
    def write_session(path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            json.dumps(
                {
                    "type": "user",
                    "message": {
                        "role": "user",
                        "content": "Inspect this ordinary project. " * 100,
                    },
                }
            )
            + "\n",
            encoding="utf-8",
        )

    def test_project_and_ancestor_names_do_not_mark_child_sessions(self) -> None:
        for directory in [
            "subagent-work/project",
            ".claude/projects/-work-subagent-library",
            "subagents/project",
        ]:
            with self.subTest(directory=directory):
                project = self.root / directory
                session = project / "main-session.jsonl"
                self.write_session(session)
                self.assertEqual(batch_analyze.find_conversation_files([project]), [session])

    def test_child_locations_and_legacy_filenames_stay_excluded(self) -> None:
        project = self.root / "project"
        main = project / "main-session.jsonl"
        self.write_session(main)
        for child in [
            "subagent/child.jsonl",
            "subagents/agent-child.jsonl",
            "subagents/nested/agent-child.jsonl",
            "subagent-run.jsonl",
        ]:
            self.write_session(project / child)
        self.assertEqual(batch_analyze.find_conversation_files([project]), [main])
        for directory in ["subagent", "subagents"]:
            self.assertEqual(batch_analyze.find_conversation_files([project / directory]), [])

    def test_size_filter_and_newest_first_order_stay_unchanged(self) -> None:
        project = self.root / "project"
        old, new, tiny = [project / name for name in ["old.jsonl", "new.jsonl", "tiny.jsonl"]]
        for session in [old, new]:
            self.write_session(session)
        tiny.write_text("{}\n", encoding="utf-8")
        os.utime(old, (100, 100))
        os.utime(new, (200, 200))
        self.assertEqual(
            batch_analyze.find_conversation_files([project, self.root / "missing"]), [new, old]
        )

    def run_cli(self, selection: list[str], home: Path) -> dict:
        analyzer = self.root / "analyzer.py"
        analyzer.write_text(
            f"#!{sys.executable}\nimport json,sys\nfrom pathlib import Path\n"
            "doc=json.loads(Path(sys.argv[1]).read_text(encoding='utf-8'))\n"
            "print(json.dumps({'received_session':doc['session_id']}))\n",
            encoding="utf-8",
        )
        analyzer.chmod(0o755)
        home.mkdir(parents=True, exist_ok=True)
        result = subprocess.run(
            [
                sys.executable,
                str(Path(batch_analyze.__file__).resolve()),
                "--dim",
                "perf",
                "--min-steps",
                "1",
                "--binary",
                str(analyzer),
                *selection,
            ],
            env={**os.environ, "HOME": str(home)},
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=20,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout.strip(), result.stderr)
        self.assertIn("Done: 1 analyzed, 0 skipped, 0 failed", result.stderr)
        output = json.loads(result.stdout)
        self.assertEqual(output["received_session"], "main-session")
        self.assertEqual(output["_session_id"], "main-session")
        return output

    @unittest.skipUnless(os.name == "posix", "executable analyzer fixtures require POSIX")
    def test_cli_explicit_directory_keeps_a_main_session(self) -> None:
        project = self.root / "subagent-project"
        session = project / "main-session.jsonl"
        self.write_session(session)
        output = self.run_cli(["--dir", str(project)], self.root / "home")
        self.assertEqual(output["_source_file"], str(session))

    @unittest.skipUnless(os.name == "posix", "executable analyzer fixtures require POSIX")
    def test_cli_default_scan_keeps_encoded_project_and_home_names(self) -> None:
        for home_name, project_name in [
            ("home", "-work-subagent-library"),
            ("subagent-workspace", "project"),
            ("subagents", "project"),
        ]:
            with self.subTest(home=home_name, project=project_name):
                home = self.root / home_name
                session = home / ".claude" / "projects" / project_name / "main-session.jsonl"
                self.write_session(session)
                output = self.run_cli([], home)
                self.assertEqual(output["_source_file"], str(session))

    @unittest.skipUnless(os.name == "posix", "executable analyzer fixtures require POSIX")
    def test_cli_explicit_file_still_bypasses_directory_filter(self) -> None:
        session = self.root / "subagents" / "main-session.jsonl"
        self.write_session(session)
        output = self.run_cli(["--file", str(session)], self.root / "home")
        self.assertEqual(output["_source_file"], str(session))


if __name__ == "__main__":
    unittest.main()
