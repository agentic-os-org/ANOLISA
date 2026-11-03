"""One physical path must occupy one batch slot despite overlapping scan roots."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from test_batch_analyze import batch_analyze


class BatchDiscoveryIdentityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.sessions = self.root / "sessions"
        self.sessions.mkdir()

    def session(self, relative: str, modified: float = 100) -> Path:
        path = self.sessions / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            json.dumps({"type": "user", "message": {"content": "fixture " * 300}}) + "\n",
            encoding="utf-8",
        )
        os.utime(path, (modified, modified))
        return path

    def test_repeated_roots_do_not_repeat_a_session(self) -> None:
        path = self.session("one.jsonl")
        self.assertEqual(
            batch_analyze.find_conversation_files([self.sessions, self.sessions]), [path]
        )

    def test_overlapping_roots_do_not_repeat_a_session(self) -> None:
        path = self.session("nested/one.jsonl")
        self.assertEqual(batch_analyze.find_conversation_files([self.root, path.parent]), [path])

    @unittest.skipUnless(os.name == "posix", "symlink fixture requires POSIX")
    def test_symlink_roots_keep_the_first_discovered_spelling(self) -> None:
        path = self.session("one.jsonl")
        alias = self.root / "alias"
        alias.symlink_to(self.sessions, target_is_directory=True)
        found = batch_analyze.find_conversation_files([alias, self.sessions])
        self.assertEqual(found, [alias / path.name])

    def test_same_basename_in_distinct_directories_remains_distinct(self) -> None:
        older = self.session("a/session.jsonl", 100)
        newer = self.session("b/session.jsonl", 200)
        self.assertEqual(batch_analyze.find_conversation_files([self.sessions]), [newer, older])

    def test_small_and_subagent_sources_keep_existing_exclusions(self) -> None:
        valid = self.session("valid.jsonl")
        self.session("subagents/child.jsonl")
        (self.sessions / "small.jsonl").write_text("{}")
        self.assertEqual(batch_analyze.find_conversation_files([self.sessions]), [valid])

    def test_disappearing_source_does_not_trigger_a_sorting_stat(self) -> None:
        path = self.session("gone.jsonl")
        original = Path.stat
        calls = []

        def metadata(candidate: Path, **kwargs: object) -> os.stat_result:
            result = original(candidate, **kwargs)
            if candidate == path:
                calls.append(candidate)
                path.unlink()
            return result

        with patch.object(Path, "stat", autospec=True, side_effect=metadata):
            found = batch_analyze.find_conversation_files([self.sessions])
        self.assertEqual(found, [path])
        self.assertEqual(len(calls), 1)

    def test_inaccessible_candidate_does_not_hide_other_sources(self) -> None:
        denied = self.session("denied.jsonl", 200)
        valid = self.session("valid.jsonl", 100)
        original = Path.stat

        def metadata(candidate: Path, **kwargs: object) -> os.stat_result:
            if candidate == denied:
                raise PermissionError("fixture inaccessible")
            return original(candidate, **kwargs)

        with patch.object(Path, "stat", autospec=True, side_effect=metadata):
            self.assertEqual(batch_analyze.find_conversation_files([self.sessions]), [valid])

    def test_missing_roots_do_not_hide_existing_sources(self) -> None:
        valid = self.session("valid.jsonl")
        self.assertEqual(
            batch_analyze.find_conversation_files([self.root / "missing", self.sessions]), [valid]
        )

    @unittest.skipUnless(os.name == "posix", "fixture executable uses a POSIX shebang")
    def test_real_cli_applies_limit_to_unique_analyzer_invocations(self) -> None:
        self.session("newest.jsonl", 200)
        self.session("older.jsonl", 100)
        counter = self.root / "calls.txt"
        executable = self.root / "analyzer"
        executable.write_text(
            f"#!{sys.executable}\nimport json, sys\nfrom pathlib import Path\na=json.loads(Path(sys.argv[1]).read_text(encoding='utf-8'))\nwith Path({str(counter)!r}).open('a') as f: f.write(a['session_id']+'\\n')\nprint(json.dumps({{'ok': True}}))\n",
            encoding="utf-8",
        )
        executable.chmod(0o700)
        result = subprocess.run(
            [
                sys.executable,
                str(batch_analyze.__file__),
                "--dir",
                str(self.sessions),
                "--dir",
                str(self.sessions),
                "--dir",
                str(self.root),
                "--limit",
                "2",
                "--dim",
                "perf",
                "--min-steps",
                "1",
                "--binary",
                str(executable),
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(counter.read_text().splitlines(), ["newest", "older"])
        self.assertIn("Found 2 conversation files", result.stderr)


if __name__ == "__main__":
    unittest.main()
