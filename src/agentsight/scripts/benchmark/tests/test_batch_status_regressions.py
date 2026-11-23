"""Preserve provider status independently of free-text tool output."""

import json
import tempfile
import unittest
from pathlib import Path

from test_batch_analyze import batch_analyze


class BatchResultStatusTests(unittest.TestCase):
    def test_deep_json_output_keeps_unknown_status_and_later_text(self) -> None:
        content = "[" * 2000 + "0" + "]" * 2000
        document = self.convert(self.result(content=content), mixed=True)
        result = document["steps"][0]["observation"]["results"][0]
        self.assertNotIn("extra", result)
        self.assertEqual(result["content"], content[:2000])
        self.assertEqual(document["steps"][1]["message"], "Next request.")

    def test_supported_declarations_survive_conversion(self) -> None:
        for fields, expected in (
            ({"isError": True}, True),
            ({"isError": False}, False),
            ({"success": False}, True),
            ({"success": True}, False),
            ({"status": "ERROR"}, True),
            ({"is_error": False, "status": "error"}, True),
            ({"success": True, "status": "error"}, True),
        ):
            with self.subTest(fields=fields):
                document = self.convert(
                    self.result(content="Error: recovered", **fields)
                )
                result = document["steps"][0]["observation"]["results"][0]
                self.assertEqual(result.get("extra"), {"is_error": expected})

    def test_json_output_declares_status_only_without_an_outer_declaration(
        self,
    ) -> None:
        for outer, nested, expected in (
            ({}, {"isError": True}, True),
            ({}, {"success": True}, False),
            ({"is_error": False}, {"isError": True}, False),
            ({"is_error": True}, {"success": True}, True),
        ):
            with self.subTest(outer=outer, nested=nested):
                # Decode before output truncation; long outputs must not lose status.
                content = json.dumps({"output": "x" * 3000, **nested})
                document = self.convert(
                    self.result(content=content, **outer), mixed=True
                )
                result = document["steps"][0]["observation"]["results"][0]
                self.assertEqual(result.get("extra"), {"is_error": expected})
                self.assertEqual(document["steps"][1]["message"], "Next request.")

    def test_non_declarations_keep_unknown_status(self) -> None:
        for fields in (
            {"success": "false"},
            {"isError": 1},
            {"status": "success"},
            {"content": "[true]"},
            {"content": "{broken"},
        ):
            with self.subTest(fields=fields):
                document = self.convert(self.result(**fields))
                result = document["steps"][0]["observation"]["results"][0]
                self.assertNotIn("extra", result)

    def convert(self, result: dict, mixed: bool = False) -> dict:
        content = [result]
        if mixed:
            content.append({"type": "text", "text": "Next request."})
        events = [
            {
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "tool_use", "id": "a", "name": "Bash", "input": {}}
                    ],
                },
            },
            {"type": "user", "message": {"role": "user", "content": content}},
        ]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "session.jsonl"
            path.write_text(
                "".join(json.dumps(event) + "\n" for event in events), encoding="utf-8"
            )
            return batch_analyze.jsonl_to_atif(path)

    def result(self, **fields: object) -> dict:
        return {
            "type": "tool_result",
            "tool_use_id": "a",
            "content": "status 1",
            **fields,
        }

    def test_failure_flag_survives_ordinary_output(self) -> None:
        document = self.convert(self.result(is_error=True))
        result = document["steps"][0]["observation"]["results"][0]
        self.assertEqual(result.get("extra"), {"is_error": True})

    def test_explicit_success_survives_error_words(self) -> None:
        document = self.convert(self.result(is_error=False, content="Error: recovered"))
        result = document["steps"][0]["observation"]["results"][0]
        self.assertEqual(result.get("extra"), {"is_error": False})

    def test_absent_flag_stays_unknown(self) -> None:
        document = self.convert(self.result())
        result = document["steps"][0]["observation"]["results"][0]
        self.assertNotIn("extra", result)

    def test_non_boolean_flags_stay_unknown(self) -> None:
        for flag in (None, 0, 1, "false", "true", [], {}):
            with self.subTest(flag=flag):
                document = self.convert(self.result(is_error=flag))
                result = document["steps"][0]["observation"]["results"][0]
                self.assertNotIn("extra", result)

    def test_mixed_reply_keeps_status_with_the_issuing_step(self) -> None:
        document = self.convert(self.result(is_error=True), mixed=True)
        self.assertEqual(document["steps"][1]["message"], "Next request.")
        result = document["steps"][0]["observation"]["results"][0]
        self.assertEqual(result.get("extra"), {"is_error": True})

    def test_nested_output_and_truncation_do_not_remove_status(self) -> None:
        document = self.convert(
            self.result(is_error=True, content=[{"type": "text", "text": "x" * 3000}])
        )
        result = document["steps"][0]["observation"]["results"][0]
        self.assertEqual(result["content"], "x" * 2000)
        self.assertEqual(result.get("extra"), {"is_error": True})


if __name__ == "__main__":
    unittest.main()
