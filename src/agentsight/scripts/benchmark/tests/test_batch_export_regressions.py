"""Exercise persistent ATIF export through the real batch CLI."""

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


class BatchExportTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.output = self.root / "exports"
        self.source = self.root / "session.jsonl"
        self.write(self.source, "检查 🐱")

    def write(self, path: Path, text: str) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            "\ufeff"
            + json.dumps({"type": "user", "message": {"content": text}}, ensure_ascii=False)
            + "\n",
            encoding="utf-8",
        )

    def cli(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        environment = {key: value for key, value in os.environ.items() if key != "OPENAI_API_KEY"}
        return subprocess.run(
            [sys.executable, batch_analyze.__file__, *arguments],
            env={**environment, "PYTHONIOENCODING": "utf-8"},
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=20,
        )

    def export(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return self.cli(
            "--file",
            str(self.source),
            "--min-steps",
            "1",
            "--export-atif",
            str(self.output),
            *arguments
        )

    def test_real_cli_exports_utf8_without_credentials_or_analyzer(self) -> None:
        original = self.source.read_bytes()
        result = self.export()
        self.assertEqual(result.returncode, 0, result.stderr)
        record = json.loads(result.stdout)
        artifact = Path(record["_atif_file"])
        self.assertEqual(artifact.parent, self.output)
        self.assertEqual(
            json.loads(artifact.read_text(encoding="utf-8")),
            batch_analyze.jsonl_to_atif(self.source),
        )
        self.assertIn("检查 🐱", artifact.read_text(encoding="utf-8"))
        self.assertEqual(self.source.read_bytes(), original)
        self.assertIn("Done: 1 exported, 0 skipped, 0 failed", result.stderr)
        self.assertFalse(list(self.output.glob(".atif-*")))

    def test_export_never_spawns_an_analyzer(self) -> None:
        arguments = [
            "batch-analyze.py",
            "--file",
            str(self.source),
            "--min-steps",
            "1",
            "--export-atif",
            str(self.output),
        ]
        with patch.object(sys, "argv", arguments), patch.object(
            batch_analyze.subprocess, "run"
        ) as run:
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(
                io.StringIO()
            ):
                batch_analyze.main()
        run.assert_not_called()
        self.assertEqual(len(list(self.output.glob("*.atif.json"))), 1)

    def test_same_basename_in_different_directories_has_distinct_artifacts(self) -> None:
        other = self.root / "nested" / self.source.name
        self.write(other, "different source")
        first = self.export()
        second = self.cli(
            "--file", str(other), "--min-steps", "1", "--export-atif", str(self.output)
        )
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertNotEqual(
            json.loads(first.stdout)["_atif_file"], json.loads(second.stdout)["_atif_file"]
        )
        self.assertEqual(len(list(self.output.glob("*.atif.json"))), 2)

    def test_repeated_export_updates_only_the_same_source_artifact(self) -> None:
        first = self.export()
        artifact = Path(json.loads(first.stdout)["_atif_file"])
        unrelated = self.output / "notes.json"
        unrelated.write_text("keep", encoding="utf-8")
        self.write(self.source, "updated source")
        second = self.export()
        self.assertEqual(json.loads(second.stdout)["_atif_file"], str(artifact))
        self.assertEqual(
            json.loads(artifact.read_text(encoding="utf-8"))["steps"][0]["message"],
            "updated source",
        )
        self.assertEqual(unrelated.read_text(encoding="utf-8"), "keep")
        self.assertEqual(len(list(self.output.glob("*.atif.json"))), 1)

    def test_directory_selection_exports_distinct_sources_with_a_limit(self) -> None:
        self.write(self.source, "first " * 300)
        self.write(self.root / "nested" / self.source.name, "second " * 300)
        self.write(self.root / "tiny.jsonl", "small")
        result = self.cli(
            "--dir",
            str(self.root),
            "--min-steps",
            "1",
            "--export-atif",
            str(self.output),
            "--limit",
            "1",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(list(self.output.glob("*.atif.json"))), 1)
        self.assertIn("Found 2 conversation files", result.stderr)
        self.assertIn("Done: 1 exported, 0 skipped, 0 failed", result.stderr)

    def test_a_write_failure_does_not_hide_later_files(self) -> None:
        other = self.root / "second.jsonl"
        self.write(other, "second")
        arguments = [
            "batch-analyze.py",
            "--dir",
            str(self.root),
            "--min-steps",
            "1",
            "--export-atif",
            str(self.output),
        ]
        actual_export = batch_analyze.export_atif
        failures = io.StringIO()
        output = io.StringIO()

        def export(document: dict, source: Path, directory: Path) -> Path:
            if source == self.source:
                raise OSError("fixture write failure")
            return actual_export(document, source, directory)

        with patch.object(sys, "argv", arguments), patch.object(
            batch_analyze, "find_conversation_files", return_value=[self.source, other]
        ), patch.object(batch_analyze, "export_atif", side_effect=export):
            with contextlib.redirect_stdout(output), contextlib.redirect_stderr(failures):
                batch_analyze.main()
        self.assertEqual(json.loads(output.getvalue())["_session_id"], "second")
        self.assertIn("Done: 1 exported, 0 skipped, 1 failed", failures.getvalue())

    def test_failed_serialization_keeps_the_existing_artifact(self) -> None:
        artifact = batch_analyze.export_atif({}, self.source, self.output)
        previous = artifact.read_bytes()
        with patch.object(batch_analyze.json, "dump", side_effect=OSError("fixture disk full")):
            with self.assertRaises(OSError):
                batch_analyze.export_atif({"changed": True}, self.source, self.output)
        self.assertEqual(artifact.read_bytes(), previous)
        self.assertFalse(list(self.output.glob(".atif-*")))

    def test_existing_minimum_step_rule_still_skips(self) -> None:
        result = self.cli("--file", str(self.source), "--export-atif", str(self.output))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertIn("Done: 0 exported, 1 skipped, 0 failed", result.stderr)
        self.assertFalse(self.output.exists())

    def test_analysis_and_export_modes_are_mutually_exclusive(self) -> None:
        result = self.export("--dim", "perf")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not allowed with argument", result.stderr)
        self.assertFalse(self.output.exists())

    def test_export_rejects_an_ignored_binary_option(self) -> None:
        result = self.export("--binary", "missing-fixture")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--binary is only supported with --dim", result.stderr)

    def test_bad_destination_is_reported_without_overwriting_it(self) -> None:
        self.output.write_text("keep", encoding="utf-8")
        result = self.export()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cannot export ATIF", result.stderr)
        self.assertIn("Done: 0 exported, 0 skipped, 1 failed", result.stderr)
        self.assertEqual(self.output.read_text(encoding="utf-8"), "keep")

    def test_atomic_replace_failure_keeps_previous_artifact_and_cleans_temporary_file(self) -> None:
        document = batch_analyze.jsonl_to_atif(self.source)
        artifact = batch_analyze.export_atif(document, self.source, self.output)
        previous = artifact.read_bytes()
        with patch.object(batch_analyze.os, "replace", side_effect=OSError("fixture failure")):
            with self.assertRaises(OSError):
                batch_analyze.export_atif({"changed": True}, self.source, self.output)
        self.assertEqual(artifact.read_bytes(), previous)
        self.assertFalse(list(self.output.glob(".atif-*")))

    def test_explicit_file_export_preserves_existing_truncation(self) -> None:
        self.write(self.source, "文" * 6000)
        result = self.export()
        self.assertEqual(result.returncode, 0, result.stderr)
        artifact = Path(json.loads(result.stdout)["_atif_file"])
        self.assertEqual(
            json.loads(artifact.read_text(encoding="utf-8"))["steps"][0]["message"], "文" * 5000
        )


if __name__ == "__main__":
    unittest.main()
