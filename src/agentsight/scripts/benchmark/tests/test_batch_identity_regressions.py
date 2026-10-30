"""Match provider echoes without stealing another tool call's observation."""

import json
import tempfile
import unittest
from pathlib import Path

from test_batch_analyze import batch_analyze
from test_batch_tool_regressions import assistant, result, text, user


class BatchCallIdentityTests(unittest.TestCase):
    def convert(self, events: list[dict]) -> dict:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "session.jsonl"
            path.write_text("".join(json.dumps(event) + "\n" for event in events), encoding="utf-8")
            return batch_analyze.jsonl_to_atif(path)

    def test_exact_id_control(self) -> None:
        document = self.convert([assistant("call_a1"), user([result("call_a1", "output")])])
        self.assertEqual(document["steps"][0]["observation"]["results"][0]["content"], "output")

    def test_ascii_case_echo_is_preserved(self) -> None:
        document = self.convert([assistant("CALL_A1"), user([result("call_a1", "output")])])
        self.assertEqual(
            document["steps"][0].get("observation", {}).get("results"),
            [{"source_call_id": "call_a1", "content": "output"}],
        )

    def test_separator_echo_in_either_direction(self) -> None:
        for call, echo in (("call_a1", "calla1"), ("calla1", "call_a1"), ("call-a1", "CALLA1")):
            with self.subTest(call=call, echo=echo):
                document = self.convert([assistant(call), user([result(echo, "output")])])
                self.assertEqual(
                    document["steps"][0].get("observation", {}).get("results"),
                    [{"source_call_id": echo, "content": "output"}],
                )

    def test_punctuated_ids_are_not_collapsed_together(self) -> None:
        document = self.convert([assistant("call_a-1"), user([result("call_a_1", "wrong call")])])
        self.assertNotIn("observation", document["steps"][0])

    def test_unknown_id_is_not_given_to_the_last_agent(self) -> None:
        document = self.convert(
            [assistant("a"), assistant("b"), user([result("missing", "orphan")])]
        )
        self.assertTrue(all("observation" not in step for step in document["steps"]))

    def test_split_steps_receive_their_own_echoes(self) -> None:
        document = self.convert(
            [
                assistant("call_a1"),
                assistant("CALL_B2"),
                user([result("calla1", "first")]),
                user([result("call_b2", "second")]),
            ]
        )
        self.assertEqual(
            [
                step.get("observation", {}).get("results", [{}])[0].get("content")
                for step in document["steps"]
            ],
            ["first", "second"],
        )

    def test_mixed_text_is_retained_when_result_uses_an_echo(self) -> None:
        document = self.convert(
            [assistant("call_a1"), user([result("calla1", "output"), text("Continue.")])]
        )
        self.assertEqual(document["steps"][1]["message"], "Continue.")
        self.assertEqual(
            document["steps"][0].get("observation", {}).get("results"),
            [{"source_call_id": "calla1", "content": "output"}],
        )
        self.assertNotIn("observation", document["steps"][1])

    def test_exact_id_wins_over_a_possible_alias(self) -> None:
        document = self.convert(
            [assistant("call_a1"), assistant("calla1"), user([result("call_a1", "exact")])]
        )
        self.assertEqual(document["steps"][0]["observation"]["results"][0]["content"], "exact")
        self.assertNotIn("observation", document["steps"][1])

    def test_ambiguous_alias_does_not_choose_a_sibling_arbitrarily(self) -> None:
        document = self.convert(
            [assistant("call_a-1"), assistant("call_a_1"), user([result("calla1", "ambiguous")])]
        )
        self.assertTrue(all("observation" not in step for step in document["steps"]))

    def test_reused_exact_id_still_selects_latest_issuer(self) -> None:
        document = self.convert([assistant("a"), assistant("a"), user([result("a", "latest")])])
        self.assertNotIn("observation", document["steps"][0])
        self.assertEqual(document["steps"][1]["observation"]["results"][0]["content"], "latest")

    def test_separator_normalization_uses_the_shared_per_character_lowercase(self) -> None:
        document = self.convert(
            [assistant("_\u039f\u03a3"), user([result("\u03bf\u03c3", "output")])]
        )
        self.assertEqual(
            document["steps"][0].get("observation", {}).get("results"),
            [{"source_call_id": "\u03bf\u03c3", "content": "output"}],
        )

    def test_later_collision_does_not_reuse_an_earlier_unique_alias(self) -> None:
        document = self.convert(
            [
                assistant("call_a-1"),
                user([result("calla1", "first")]),
                assistant("call_a_1"),
                user([result("calla1", "now ambiguous")]),
            ]
        )
        self.assertEqual(
            document["steps"][0]["observation"]["results"],
            [{"source_call_id": "calla1", "content": "first"}],
        )
        self.assertNotIn("observation", document["steps"][1])

    def test_reused_call_id_keeps_alias_lookup_on_the_latest_issuer(self) -> None:
        document = self.convert(
            [assistant("call_a1"), assistant("call_a1"), user([result("calla1", "latest")])]
        )
        self.assertNotIn("observation", document["steps"][0])
        self.assertEqual(
            document["steps"][1].get("observation", {}).get("results"),
            [{"source_call_id": "calla1", "content": "latest"}],
        )

    def test_non_ascii_case_is_not_an_ascii_case_alias(self) -> None:
        document = self.convert([assistant("\u00c4A"), user([result("\u00e4a", "different ID")])])
        self.assertNotIn("observation", document["steps"][0])

    def test_ascii_case_can_change_around_an_unchanged_non_ascii_character(self) -> None:
        document = self.convert([assistant("\u00c4_CALL"), user([result("\u00c4_call", "output")])])
        self.assertEqual(
            document["steps"][0].get("observation", {}).get("results"),
            [{"source_call_id": "\u00c4_call", "content": "output"}],
        )


if __name__ == "__main__":
    unittest.main()
