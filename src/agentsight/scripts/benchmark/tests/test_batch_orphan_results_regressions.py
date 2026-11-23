"""Unowned tool evidence must survive without acquiring conversation semantics."""

import json
import tempfile
import unittest
from pathlib import Path

from test_batch_analyze import batch_analyze
from test_batch_tool_regressions import assistant, result, text, user


class BatchOrphanResultsTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.source = Path(directory.name) / "partial.jsonl"

    def convert(self, events: list[dict]) -> dict:
        self.source.write_text(
            "".join(json.dumps(event) + "\n" for event in events), encoding="utf-8"
        )
        return batch_analyze.jsonl_to_atif(self.source)

    def test_result_only_transcript_is_exportable_without_a_fabricated_turn(self) -> None:
        block = result("before-compaction", "permission denied")
        block["is_error"] = True
        event = user([block])
        event["timestamp"] = "2026-01-01T00:00:08Z"
        document = self.convert([event])
        self.assertEqual(document["steps"], [])
        expected = [
            {
                "timestamp": event["timestamp"],
                "results": [
                    {
                        "source_call_id": "before-compaction",
                        "content": "permission denied",
                        "extra": {"is_error": True},
                    }
                ],
            }
        ]
        self.assertEqual(document["extra"]["unmatched_tool_results"], expected)
        exported = batch_analyze.export_atif(document, self.source, self.source.parent / "export")
        self.assertEqual(json.loads(exported.read_text(encoding="utf-8")), document)

    def test_known_and_unknown_results_are_each_retained_once(self) -> None:
        document = self.convert(
            [
                assistant("known"),
                user([result("known", "known output"), result("missing", "other output")]),
                user([text("real question")]),
            ]
        )
        self.assertEqual([step["source"] for step in document["steps"]], ["agent", "user"])
        self.assertEqual([step["step_id"] for step in document["steps"]], [1, 2])
        self.assertEqual(
            document["steps"][0]["observation"]["results"],
            [
                {"source_call_id": "known", "content": "known output"},
            ],
        )
        self.assertEqual(
            document["extra"]["unmatched_tool_results"],
            [
                {
                    "timestamp": None,
                    "results": [{"source_call_id": "missing", "content": "other output"}],
                },
            ],
        )
        self.assertEqual(document["steps"][1]["message"], "real question")

    def test_mixed_reply_keeps_only_genuine_text_in_the_user_turn(self) -> None:
        document = self.convert([user([result("missing", "error output"), text("try again")])])
        self.assertEqual(len(document["steps"]), 1)
        step = document["steps"][0]
        self.assertEqual(step["source"], "user")
        self.assertEqual(step["message"], "try again")
        self.assertNotIn("extra", document)
        self.assertEqual(
            step["observation"]["results"],
            [
                {"source_call_id": "missing", "content": "error output"},
            ],
        )

    def test_multiple_records_retain_order_and_status_without_an_owner(self) -> None:
        first = result("absent", "failed")
        first["is_error"] = True
        second = result("absent", "recovered")
        second["is_error"] = False
        missing_id = {"type": "tool_result", "content": "unidentified output"}
        document = self.convert([user([first]), user([second, missing_id])])
        self.assertEqual(document["steps"], [])
        groups = document["extra"]["unmatched_tool_results"]
        self.assertEqual([len(group["results"]) for group in groups], [1, 2])
        results = [entry for group in groups for entry in group["results"]]
        self.assertEqual(
            [entry["content"] for entry in results],
            [
                "failed",
                "recovered",
                "unidentified output",
            ],
        )
        self.assertEqual(
            [entry.get("extra") for entry in results],
            [
                {"is_error": True},
                {"is_error": False},
                None,
            ],
        )
        self.assertIsNone(results[-1]["source_call_id"])

    def test_known_results_do_not_create_unmatched_metadata(self) -> None:
        document = self.convert([assistant("known"), user([result("known", "output")])])
        self.assertNotIn("extra", document)
        self.assertEqual(len(document["steps"]), 1)
        self.assertEqual(document["steps"][0]["observation"]["results"][0]["content"], "output")

    def test_empty_or_metadata_only_input_is_still_skipped(self) -> None:
        self.assertIsNone(self.convert([]))
        self.assertIsNone(self.convert([{"type": "summary"}, user([text(" ")])]))
