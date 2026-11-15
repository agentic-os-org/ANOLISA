"""Temporary input I/O failures must stay local to one batch item."""

import errno
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from test_batch_analyze import batch_analyze


class TemporaryInputTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.sessions = [self.root / f"{name}.jsonl" for name in ("first", "second")]
        for path in self.sessions:
            path.write_text(
                json.dumps({"type": "user", "message": {"content": "Inspect this."}})
                + "\n",
                encoding="utf-8",
            )

    def run_items(self, jobs: int = 1) -> list:
        with patch.object(
            batch_analyze.subprocess,
            "run",
            return_value=subprocess.CompletedProcess([], 0, stdout="{}", stderr=""),
        ):
            return list(
                batch_analyze.analyze_ordered(
                    self.sessions, jobs, 1, ["analyze"], "perf"
                )
            )

    def test_creation_failure_does_not_abort_the_remaining_sessions(self) -> None:
        create = tempfile.NamedTemporaryFile
        paths = []
        attempts = 0

        def temporary(**kwargs: object) -> object:
            nonlocal attempts
            attempts += 1
            if attempts == 1:
                raise OSError(errno.ENOSPC, "No space left on device")
            stream = create(dir=self.root, **kwargs)
            paths.append(Path(stream.name))
            return stream

        with patch.object(
            batch_analyze.tempfile, "NamedTemporaryFile", side_effect=temporary
        ):
            results = self.run_items()
        self.assertEqual([item[0] for item in results], ["failed", "analyzed"])
        self.assertIn("temporary", results[0][2])
        self.assertEqual(results[1][1]["_session_id"], "second")
        self.assertTrue(all(not path.exists() for path in paths))

    def test_partial_write_is_cleaned_and_other_workers_finish(self) -> None:
        dump = json.dump
        create = tempfile.NamedTemporaryFile

        def write(document: dict, stream: object, **kwargs: object) -> None:
            if document["session_id"] == "first":
                stream.write("{")
                raise OSError(errno.ENOSPC, "No space left on device")
            dump(document, stream, **kwargs)

        for jobs in (1, 2):
            with self.subTest(jobs=jobs), patch.object(
                batch_analyze.json, "dump", side_effect=write
            ), patch.object(
                batch_analyze.tempfile,
                "NamedTemporaryFile",
                side_effect=lambda **kwargs: create(dir=self.root, **kwargs),
            ):
                results = self.run_items(jobs)
                self.assertEqual([item[0] for item in results], ["failed", "analyzed"])
                self.assertIsNone(results[0][1])
                self.assertEqual(results[1][1]["_session_id"], "second")
                self.assertEqual(list(self.root.glob("atif-*.json")), [])

    def test_cleanup_failure_preserves_completed_analysis_and_diagnostics(self) -> None:
        create = tempfile.NamedTemporaryFile
        paths = []

        def temporary(**kwargs: object) -> object:
            stream = create(dir=self.root, **kwargs)
            paths.append(Path(stream.name))
            return stream

        with patch.object(
            batch_analyze.tempfile, "NamedTemporaryFile", side_effect=temporary
        ), patch.object(
            batch_analyze.os,
            "unlink",
            side_effect=PermissionError("temporarily locked"),
        ):
            results = self.run_items()
        self.assertEqual([item[0] for item in results], ["analyzed", "analyzed"])
        self.assertTrue(all("WARN" in item[2] for item in results))
        for path in paths:
            path.unlink()


if __name__ == "__main__":
    unittest.main()
