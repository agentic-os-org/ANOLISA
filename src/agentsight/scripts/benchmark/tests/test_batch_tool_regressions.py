"""Keep tool outputs with their calls and genuine user text with its own turn."""

import json
import tempfile
import unittest
from pathlib import Path

from test_batch_analyze import batch_analyze


def user(content: object) -> dict:
    return {"type": "user", "message": {"role": "user", "content": content}}


def assistant(*identifiers: str) -> dict:
    return {
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [
                {"type": "tool_use", "id": identifier, "name": "read_file", "input": {}}
                for identifier in identifiers
            ],
        },
    }


def text(value: str) -> dict:
    return {"type": "text", "text": value}


def result(identifier: str, value: object) -> dict:
    return {"type": "tool_result", "tool_use_id": identifier, "content": value}


class BatchToolReplyTests(unittest.TestCase):
    def convert(self, events: list[dict]) -> dict:
        with tempfile.TemporaryDirectory() as directory:
            session = Path(directory) / "session.jsonl"
            session.write_text(
                "".join(json.dumps(event) + "\n" for event in events), encoding="utf-8"
            )
            return batch_analyze.jsonl_to_atif(session)

    def test_parallel_results_append_without_losing_earlier_results(self) -> None:
        document = self.convert(
            [assistant("a", "b"), user([result("a", "first")]), user([result("b", "second")])]
        )
        self.assertEqual(
            [item["source_call_id"] for item in document["steps"][0]["observation"]["results"]],
            ["a", "b"],
        )

    def test_mixed_reply_preserves_human_text_and_observation(self) -> None:
        document = self.convert(
            [assistant("a"), user([result("a", "tool output"), text("Next request.")])]
        )
        self.assertEqual([step["source"] for step in document["steps"]], ["agent", "user"])
        self.assertEqual(document["steps"][1]["message"], "Next request.")
        self.assertEqual(
            document["steps"][0]["observation"]["results"][0]["content"], "tool output"
        )

    def test_split_assistant_blocks_route_to_the_issuing_call(self) -> None:
        document = self.convert(
            [
                assistant("a"),
                assistant("b"),
                user([result("a", "first")]),
                user([result("b", "second")]),
            ]
        )
        first, second = document["steps"]
        self.assertEqual(
            first.get("observation", {}).get("results"),
            [{"source_call_id": "a", "content": "first"}],
        )
        self.assertEqual(
            second["observation"]["results"], [{"source_call_id": "b", "content": "second"}]
        )

    def test_intervening_user_turn_does_not_receive_tool_stdout_as_text(self) -> None:
        document = self.convert(
            [assistant("a"), user("New instruction."), user([result("a", "tool output")])]
        )
        self.assertEqual([step["source"] for step in document["steps"]], ["agent", "user"])
        self.assertEqual(document["steps"][1]["message"], "New instruction.")
        self.assertEqual(
            document["steps"][0]["observation"]["results"][0]["content"], "tool output"
        )

    def test_nested_orphan_result_neither_aborts_nor_becomes_human_text(self) -> None:
        document = self.convert(
            [
                user([result("missing", [text("tool output")])]),
                user("Actual human request."),
                assistant("a"),
            ]
        )
        users = [step["message"] for step in document["steps"] if step["source"] == "user"]
        self.assertEqual(users, ["Actual human request."])

    def test_unmatched_result_in_a_mixed_reply_stays_an_observation(self) -> None:
        document = self.convert(
            [user([result("missing", "tool output"), text("Actual human request.")])]
        )
        step = document["steps"][0]
        self.assertEqual(step["message"], "Actual human request.")
        self.assertEqual(
            step["observation"]["results"],
            [{"source_call_id": "missing", "content": "tool output"}],
        )

    def test_mixed_known_and_unknown_results_keep_separate_owners(self) -> None:
        document = self.convert(
            [
                assistant("a"),
                user([result("a", "owned"), result("missing", "unmatched"), text("Next request.")]),
            ]
        )
        self.assertEqual(
            document["steps"][0]["observation"]["results"],
            [{"source_call_id": "a", "content": "owned"}],
        )
        self.assertEqual(
            document["steps"][1]["observation"]["results"],
            [{"source_call_id": "missing", "content": "unmatched"}],
        )

    def test_existing_tool_output_truncation_is_preserved(self) -> None:
        document = self.convert([assistant("a"), user([result("a", "x" * 3000)])])
        self.assertEqual(document["steps"][0]["observation"]["results"][0]["content"], "x" * 2000)

    def test_human_text_helper_keeps_its_existing_default_contract(self) -> None:
        self.assertEqual(
            batch_analyze.extract_text([text("human"), result("a", "output")]), "human\noutput"
        )


if __name__ == "__main__":
    unittest.main()
