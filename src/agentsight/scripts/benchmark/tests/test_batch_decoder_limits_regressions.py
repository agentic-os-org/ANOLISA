"""Regression coverage for JSON decoder safety limits in batch-analyze.

``json.loads`` raises RecursionError for deeply nested arrays and ValueError
for integers with more digits than Python 3.11+ permits.  ``_read_entries``
used to catch only JSONDecodeError, so one rejected record aborted the whole
conversion or CLI run instead of being skipped like malformed JSON.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from test_batch_analyze import batch_analyze


VALID_FIRST = json.dumps({"type": "user", "message": {"role": "user", "content": "hello"}})
VALID_SECOND = json.dumps({"type": "user", "message": {"role": "user", "content": "world"}})
RECURSION_RECORD = "[" * 2000 + "]" * 2000
INT_LIMIT_RECORD = "1" + "0" * 5000


class BatchDecoderLimitConverterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.source = Path(self.directory.name) / "session.jsonl"

    def write(self, lines: list[str]) -> None:
        self.source.write_text("\n".join(lines) + "\n", encoding="utf-8")

    def messages(self, doc: dict) -> list[str]:
        self.assertIsNotNone(doc)
        return [step["message"] for step in doc["steps"]]

    def test_recursion_record_before_during_and_after_conversation_is_skipped(self) -> None:
        for position in range(3):
            lines = [VALID_FIRST, VALID_SECOND]
            lines.insert(position, RECURSION_RECORD)
            self.write(lines)
            self.assertEqual(
                self.messages(batch_analyze.jsonl_to_atif(self.source)),
                ["hello", "world"],
            )

    def test_oversized_integer_before_during_and_after_conversation_is_skipped(self) -> None:
        for position in range(3):
            lines = [VALID_FIRST, VALID_SECOND]
            lines.insert(position, INT_LIMIT_RECORD)
            self.write(lines)
            self.assertEqual(
                self.messages(batch_analyze.jsonl_to_atif(self.source)),
                ["hello", "world"],
            )

    def test_valid_nested_metadata_control_still_passes(self) -> None:
        metadata = json.dumps(
            {
                "type": "runtime-config",
                "sessionId": "session-a",
                "model": "control-model",
                "version": "1.0.0",
                "entrypoint": "claude-code",
            }
        )
        self.write([metadata, VALID_FIRST])
        doc = batch_analyze.jsonl_to_atif(self.source)
        self.assertIsNotNone(doc)
        self.assertEqual(doc["agent"]["name"], "claude-code")
        self.assertEqual(doc["session_id"], "session-a")


class BatchDecoderLimitCliTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.source = self.root / "session.jsonl"
        self.output = self.root / "exports"

    def write(self, lines: list[str]) -> None:
        self.source.write_text("\n".join(lines) + "\n", encoding="utf-8")

    def run_export(self) -> subprocess.CompletedProcess[str]:
        environment = {
            key: value for key, value in os.environ.items() if key != "OPENAI_API_KEY"
        }
        return subprocess.run(
            [
                sys.executable,
                batch_analyze.__file__,
                "--file",
                str(self.source),
                "--min-steps",
                "1",
                "--export-atif",
                str(self.output),
            ],
            env={**environment, "PYTHONIOENCODING": "utf-8"},
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=20,
        )

    def test_cli_preserves_neighboring_records_for_decoder_limits(self) -> None:
        self.write([VALID_FIRST, RECURSION_RECORD, VALID_SECOND, INT_LIMIT_RECORD])
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Done: 1 exported, 0 skipped, 0 failed", result.stderr)
        artifact = Path(json.loads(result.stdout)["_atif_file"])
        document = json.loads(artifact.read_text(encoding="utf-8"))
        self.assertEqual(
            [step["message"] for step in document["steps"]],
            ["hello", "world"],
        )


if __name__ == "__main__":
    unittest.main()
