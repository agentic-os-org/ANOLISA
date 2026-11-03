"""Claude-style input counters must reach batch analysis with inclusive totals."""

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


class BatchUsageTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.session = Path(self.directory.name) / "session.jsonl"

    def write(self, usage: object) -> None:
        event = {
            "type": "assistant",
            "message": {
                "role": "assistant",
                "usage": usage,
                "content": [{"type": "text", "text": "Project inspected."}],
            },
        }
        self.session.write_text(json.dumps(event) + "\n", encoding="utf-8")

    def metrics(self, usage: object) -> dict[str, int]:
        self.write(usage)
        document = batch_analyze.jsonl_to_atif(self.session)
        return document["steps"][0].get("metrics", {})

    def test_prompt_includes_cache_read_and_creation(self) -> None:
        metrics = self.metrics(
            {
                "input_tokens": 1000,
                "cache_read_input_tokens": 200,
                "cache_creation_input_tokens": 300,
                "output_tokens": 50,
            }
        )
        self.assertEqual(
            metrics, {"prompt_tokens": 1500, "completion_tokens": 50, "cached_tokens": 200}
        )

    def test_cache_reads_smaller_than_uncached_input_are_still_included(self) -> None:
        metrics = self.metrics({"input_tokens": 1000, "cache_read_input_tokens": 200})
        self.assertEqual(metrics["prompt_tokens"], 1200)

    def test_cache_creation_is_input_even_without_a_read(self) -> None:
        metrics = self.metrics({"input_tokens": 1000, "cache_creation_input_tokens": 300})
        self.assertEqual(metrics["prompt_tokens"], 1300)
        self.assertEqual(metrics.get("cached_tokens", 0), 0)

    def test_zero_uncached_input_keeps_the_cached_prompt_total(self) -> None:
        metrics = self.metrics(
            {
                "input_tokens": 0,
                "cache_read_input_tokens": 1000,
                "cache_creation_input_tokens": 900,
                "output_tokens": 5,
            }
        )
        self.assertEqual(metrics["prompt_tokens"], 1900)
        self.assertEqual(metrics["cached_tokens"], 1000)

    def test_no_cache_control_keeps_existing_counts(self) -> None:
        self.assertEqual(
            self.metrics({"input_tokens": 1000, "output_tokens": 50}),
            {"prompt_tokens": 1000, "completion_tokens": 50},
        )

    def test_explicit_zeros_are_not_missing_measurements(self) -> None:
        self.assertEqual(
            self.metrics({"input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0}),
            {"prompt_tokens": 0, "completion_tokens": 0, "cached_tokens": 0},
        )

    def test_missing_input_does_not_invent_a_known_prompt_total(self) -> None:
        metrics = self.metrics({"cache_read_input_tokens": 200, "output_tokens": 50})
        self.assertNotIn("prompt_tokens", metrics)
        self.assertEqual(metrics["cached_tokens"], 200)
        self.assertEqual(metrics["completion_tokens"], 50)

    def test_invalid_counters_are_not_promoted_or_added(self) -> None:
        metrics = self.metrics(
            {
                "input_tokens": True,
                "cache_read_input_tokens": "200",
                "cache_creation_input_tokens": -10,
                "output_tokens": 50,
            }
        )
        self.assertEqual(metrics, {"completion_tokens": 50})
        metrics = self.metrics(
            {
                "input_tokens": 1000,
                "cache_read_input_tokens": None,
                "cache_creation_input_tokens": -10,
                "output_tokens": 50,
            }
        )
        self.assertEqual(metrics, {"prompt_tokens": 1000, "completion_tokens": 50})

    def test_malformed_usage_container_does_not_abort_a_batch(self) -> None:
        for usage in (None, [], ["unexpected"], "unexpected"):
            with self.subTest(usage=usage):
                self.assertEqual(self.metrics(usage), {})

    def test_cli_passes_normalized_counts_to_cost_analysis(self) -> None:
        self.write(
            {
                "input_tokens": 1000,
                "cache_read_input_tokens": 200,
                "cache_creation_input_tokens": 300,
                "output_tokens": 50,
            }
        )
        observed = []

        def analyze(arguments: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
            observed.append(json.loads(Path(arguments[1]).read_text()))
            return subprocess.CompletedProcess(arguments, 0, stdout="{}", stderr="")

        arguments = [
            "batch-analyze.py",
            "--file",
            str(self.session),
            "--dim",
            "cost",
            "--min-steps",
            "1",
            "--binary",
            "fixture-analyzer",
        ]
        with patch.object(sys, "argv", arguments), patch.object(
            batch_analyze.subprocess, "run", analyze
        ):
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(
                io.StringIO()
            ):
                batch_analyze.main()
        self.assertEqual(len(observed), 1)
        self.assertEqual(observed[0]["steps"][0]["metrics"]["prompt_tokens"], 1500)


if __name__ == "__main__":
    unittest.main()
