"""Incomplete trailing records must not erase the last measured usage."""

import contextlib
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

from test_batch_analyze import batch_analyze


class SnapshotRetentionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "session.jsonl"

    def write(self, records: list[tuple[str | None, object]]) -> dict[str, Any]:
        self.path.write_text(
            "".join(
                json.dumps(
                    {
                        "type": "assistant",
                        "message": {
                            "id": identity,
                            "usage": usage,
                            "content": [{"type": "text", "text": str(i)}],
                        },
                    }
                )
                + "\n"
                for i, (identity, usage) in enumerate(records)
            ),
            encoding="utf-8",
        )
        return batch_analyze.jsonl_to_atif(self.path)

    def test_empty_or_invalid_tail_retains_last_valid_snapshot(self) -> None:
        for tail in (
            {},
            {"service_tier": "standard"},
            {"input_tokens": None},
            {"input_tokens": True, "output_tokens": -1},
            None,
            [],
        ):
            with self.subTest(tail=tail):
                document = self.write(
                    [
                        ("m", {"input_tokens": 100, "output_tokens": 1}),
                        (
                            "m",
                            {
                                "input_tokens": 100,
                                "output_tokens": 9,
                                "cache_read_input_tokens": 20,
                            },
                        ),
                        ("m", tail),
                    ]
                )
                self.assertNotIn("metrics", document["steps"][0])
                self.assertEqual(
                    document["steps"][1]["metrics"],
                    {"prompt_tokens": 120, "completion_tokens": 9, "cached_tokens": 20},
                )
                self.assertNotIn("metrics", document["steps"][2])
                self.assertEqual([s["message"] for s in document["steps"]], ["0", "1", "2"])

    def test_known_zero_replaces_old_snapshot(self) -> None:
        document = self.write(
            [("m", {"input_tokens": 100}), ("m", {"input_tokens": 0, "output_tokens": 0})]
        )
        self.assertNotIn("metrics", document["steps"][0])
        self.assertEqual(
            document["steps"][1]["metrics"], {"prompt_tokens": 0, "completion_tokens": 0}
        )

    def test_independent_ids_and_anonymous_measurements_remain_separate(self) -> None:
        document = self.write(
            [
                ("a", {"input_tokens": 10}),
                ("b", {"input_tokens": 20}),
                (None, {"input_tokens": 30}),
                (None, {}),
                ("a", {}),
            ]
        )
        self.assertEqual(
            sum(s.get("metrics", {}).get("prompt_tokens", 0) for s in document["steps"]), 60
        )

    def test_cached_only_measurement_remains_available(self) -> None:
        document = self.write([("m", {"cache_read_input_tokens": 20}), ("m", {})])
        self.assertEqual(document["steps"][0]["metrics"], {"cached_tokens": 20})

    def test_export_and_cli_keep_the_retained_snapshot(self) -> None:
        document = self.write([("m", {"input_tokens": 100, "output_tokens": 9}), ("m", {})])
        exported = batch_analyze.export_atif(document, self.path, self.path.parent / "exports")
        self.assertEqual(
            json.loads(exported.read_text())["steps"][0]["metrics"]["prompt_tokens"], 100
        )
        observed = []

        def analyze(arguments: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
            observed.append(json.loads(Path(arguments[1]).read_text()))
            return subprocess.CompletedProcess(arguments, 0, stdout="{}", stderr="")

        argv = [
            "batch-analyze.py",
            "--file",
            str(self.path),
            "--dim",
            "cost",
            "--min-steps",
            "1",
            "--binary",
            "fixture-analyzer",
        ]
        with patch.object(sys, "argv", argv), patch.object(
            batch_analyze.subprocess, "run", analyze
        ):
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(
                io.StringIO()
            ):
                batch_analyze.main()
        self.assertEqual(len(observed), 1)
        self.assertEqual(observed[0]["steps"], document["steps"])


if __name__ == "__main__":
    unittest.main()
