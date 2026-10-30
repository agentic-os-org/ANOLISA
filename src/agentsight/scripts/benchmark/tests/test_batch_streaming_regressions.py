"""Metadata-heavy logs should not be buffered alongside converted ATIF steps."""

import io
import json
import tempfile
import tracemalloc
import unittest
from collections.abc import Iterator
from pathlib import Path
from unittest.mock import patch

from test_batch_analyze import batch_analyze


class BatchStreamingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "session.jsonl"

    def write(self, records: list[dict]) -> None:
        self.path.write_text(
            "".join(json.dumps(record, ensure_ascii=False) + "\n" for record in records),
            encoding="utf-8",
        )

    def test_ignored_history_does_not_accumulate_in_memory(self) -> None:
        metadata = json.dumps({"type": "file-history-snapshot", "payload": "x" * 2048})
        with self.path.open("w", encoding="utf-8") as stream:
            for _ in range(5000):
                stream.write(metadata + "\n")
            stream.write('{"type":"user","message":{"content":"hello"}}\n')
        tracemalloc.start()
        try:
            document = batch_analyze.jsonl_to_atif(self.path)
            _, peak = tracemalloc.get_traced_memory()
        finally:
            tracemalloc.stop()
        self.assertEqual(document["steps"][0]["message"], "hello")
        self.assertLess(peak, 3 * 1024 * 1024, f"ignored records retained {peak} bytes")

    def test_later_results_and_usage_snapshots_keep_their_owners(self) -> None:
        self.write(
            [
                {
                    "type": "assistant",
                    "message": {
                        "id": "shared",
                        "content": [
                            {"type": "tool_use", "id": "call_a", "name": "read", "input": {}}
                        ],
                        "usage": {"input_tokens": 10, "output_tokens": 2},
                    },
                },
                {"type": "file-history-snapshot", "payload": "ignored"},
                {"type": "user", "message": {"content": "continue"}},
                {
                    "type": "assistant",
                    "message": {
                        "id": "shared",
                        "content": "检查 🐱",
                        "usage": {"input_tokens": 20, "output_tokens": 3},
                    },
                },
                {
                    "type": "user",
                    "message": {
                        "content": [
                            {
                                "type": "tool_result",
                                "tool_use_id": "CALLA",
                                "content": "Error: recovered",
                                "is_error": False,
                            }
                        ]
                    },
                },
            ]
        )
        steps = batch_analyze.jsonl_to_atif(self.path)["steps"]
        self.assertEqual([step["source"] for step in steps], ["agent", "user", "agent"])
        self.assertNotIn("metrics", steps[0])
        self.assertEqual(steps[2]["metrics"], {"prompt_tokens": 20, "completion_tokens": 3})
        self.assertEqual(
            steps[0]["observation"]["results"],
            [
                {
                    "source_call_id": "CALLA",
                    "content": "Error: recovered",
                    "extra": {"is_error": False},
                }
            ],
        )
        self.assertEqual(steps[2]["message"], "检查 🐱")

    def test_bom_and_malformed_records_keep_their_policy(self) -> None:
        self.path.write_text(
            '\ufeff{"type":"runtime-config","model":"fixture-model"}\n'
            "\nnot JSON\n42\n[]\n"
            '{"type":"user","message":{"content":"keep \ufeff inside"}}\n',
            encoding="utf-8",
        )
        document = batch_analyze.jsonl_to_atif(self.path)
        self.assertEqual(document["agent"]["model_name"], "fixture-model")
        self.assertEqual([step["message"] for step in document["steps"]], ["keep \ufeff inside"])

    def test_late_read_failure_returns_no_partial_document_and_closes(self) -> None:
        class BrokenStream(io.StringIO):
            def __iter__(self) -> Iterator[str]:
                yield '{"type":"user","message":{"content":"partial"}}\n'
                raise OSError("fixture read failure")

        stream = BrokenStream()
        with patch("builtins.open", return_value=stream):
            self.assertIsNone(batch_analyze.jsonl_to_atif(self.path))
        self.assertTrue(stream.closed)

    def test_missing_source_remains_unreadable(self) -> None:
        self.assertIsNone(batch_analyze.jsonl_to_atif(self.path))

    def test_conversion_exception_closes_the_reader_before_it_propagates(self) -> None:
        record = {"type": "assistant", "message": {"content": "valid text"}}
        stream = io.StringIO(json.dumps(record) + '\n{"type":"mode"}\n')
        with patch("builtins.open", return_value=stream), patch.object(
            batch_analyze, "extract_text", side_effect=RuntimeError("fixture conversion failure")
        ):
            try:
                batch_analyze.jsonl_to_atif(self.path)
            except RuntimeError as error:
                # Retain the traceback while checking: it holds the converter frame.
                self.assertIsNotNone(error.__traceback__)
                self.assertTrue(stream.closed)
            else:
                self.fail("existing conversion exceptions must still propagate")

    def test_empty_or_metadata_only_source_produces_no_trajectory(self) -> None:
        for records in ([], [{"type": "mode"}], [{"type": "file-history-snapshot"}]):
            with self.subTest(records=records):
                self.write(records)
                self.assertIsNone(batch_analyze.jsonl_to_atif(self.path))

    def test_required_output_is_retained_and_truncated_as_before(self) -> None:
        content = "界" * 6000
        self.write([{"type": "user", "message": {"content": content}} for _ in range(2000)])
        steps = batch_analyze.jsonl_to_atif(self.path)["steps"]
        self.assertEqual(len(steps), 2000)
        self.assertEqual(steps[-1]["step_id"], 2000)
        self.assertTrue(all(step["message"] == content[:5000] for step in steps))


if __name__ == "__main__":
    unittest.main()
