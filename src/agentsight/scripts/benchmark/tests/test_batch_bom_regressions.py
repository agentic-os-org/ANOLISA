"""File-boundary encoding regressions through the real batch converter and CLI."""

import contextlib
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from test_batch_analyze import batch_analyze


class BatchBomTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.session = Path(self.directory.name) / "session.jsonl"
        self.events = [
            {"type": "user", "message": {"role": "user", "content": "Inspect the project."}},
            {
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "Project inspected."}],
                },
            },
        ]

    def write(self, prefix: str = "") -> None:
        self.session.write_text(
            prefix + "".join(json.dumps(event, ensure_ascii=False) + "\n" for event in self.events),
            encoding="utf-8",
        )

    def test_plain_input_keeps_both_turns(self) -> None:
        self.write()
        document = batch_analyze.jsonl_to_atif(self.session)
        self.assertEqual([step["source"] for step in document["steps"]], ["user", "agent"])

    def test_one_bom_preserves_the_first_user_record(self) -> None:
        self.write()
        plain = batch_analyze.jsonl_to_atif(self.session)
        self.write("\ufeff")
        self.assertEqual(batch_analyze.jsonl_to_atif(self.session), plain)

    def test_one_bom_preserves_runtime_configuration(self) -> None:
        self.events.insert(0, {"type": "runtime-config", "model": "fixture-model"})
        self.write("\ufeff")
        document = batch_analyze.jsonl_to_atif(self.session)
        self.assertEqual(document["agent"]["model_name"], "fixture-model")

    def test_literal_marker_inside_message_is_preserved(self) -> None:
        self.events[0]["message"]["content"] = "left\ufeffright"
        self.write("\ufeff")
        document = batch_analyze.jsonl_to_atif(self.session)
        self.assertEqual(document["steps"][0]["message"], "left\ufeffright")

    def test_a_second_leading_marker_remains_malformed_data(self) -> None:
        self.write("\ufeff\ufeff")
        document = batch_analyze.jsonl_to_atif(self.session)
        self.assertEqual([step["source"] for step in document["steps"]], ["agent"])

    def test_a_marker_on_a_later_line_is_not_an_encoding_prefix(self) -> None:
        self.session.write_text(
            json.dumps(self.events[0]) + "\n\ufeff" + json.dumps(self.events[1]) + "\n",
            encoding="utf-8",
        )
        document = batch_analyze.jsonl_to_atif(self.session)
        self.assertEqual([step["source"] for step in document["steps"]], ["user"])

    def test_cli_sends_the_complete_marked_session_to_analysis(self) -> None:
        self.write("\ufeff")
        observed = []

        def analyze(arguments: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
            observed.append(json.loads(Path(arguments[1]).read_text()))
            return subprocess.CompletedProcess(arguments, 0, stdout="{}", stderr="")

        arguments = [
            "batch-analyze.py",
            "--file",
            str(self.session),
            "--dim",
            "perf",
            "--min-steps",
            "2",
            "--binary",
            "fixture-analyzer",
        ]
        output = io.StringIO()
        with patch.object(sys, "argv", arguments), patch.object(
            batch_analyze.subprocess, "run", analyze
        ):
            with contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO()):
                batch_analyze.main()
        self.assertEqual(
            len(observed), 1, "a BOM must not make a valid session fall below min-steps"
        )
        self.assertEqual(json.loads(output.getvalue())["_steps"], 2)
        self.assertEqual(observed[0]["steps"][0]["message"], "Inspect the project.")


if __name__ == "__main__":
    unittest.main()
