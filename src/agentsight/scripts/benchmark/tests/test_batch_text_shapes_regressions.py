"""Malformed text fields must not discard valid neighboring batch evidence."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from test_batch_analyze import batch_analyze


class BatchTextShapeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def write_session(self, name: str, entries: list[dict]) -> Path:
        path = self.root / name
        path.write_text("\n".join(json.dumps(entry) for entry in entries), encoding="utf-8")
        return path

    def test_invalid_text_fields_preserve_valid_neighbors(self) -> None:
        for invalid in (None, False, 12, [], {}):
            with self.subTest(invalid=invalid):
                content = [{"type": "text", "text": invalid}, {"type": "text", "text": "valid"}]
                self.assertEqual(batch_analyze.extract_text(content), "valid")

    def test_invalid_thinking_fields_preserve_valid_reasoning(self) -> None:
        for invalid in (None, True, 12, [], {}):
            with self.subTest(invalid=invalid):
                content = [
                    {"type": "thinking", "thinking": invalid},
                    {"type": "thinking", "thinking": "reason"},
                ]
                self.assertEqual(batch_analyze.extract_thinking(content), "reason")

    def test_nested_result_text_is_filtered_without_losing_attribution(self) -> None:
        content = [
            {
                "type": "tool_result",
                "tool_use_id": "call-1",
                "is_error": False,
                "content": [
                    {"type": "text", "text": None},
                    {"type": "text", "text": "saved"},
                    {"type": "image", "source": {}},
                    {"type": "text", "text": []},
                ],
            }
        ]
        result = batch_analyze.extract_tool_results(content)[0]
        self.assertEqual(
            result, {"source_call_id": "call-1", "content": "saved ", "extra": {"is_error": False}}
        )

    def test_extract_text_accepts_structured_tool_results(self) -> None:
        content = [
            {"type": "text", "text": "answer"},
            {"type": "tool_result", "content": [{"type": "text", "text": "tool text"}]},
        ]
        self.assertEqual(batch_analyze.extract_text(content), "answer\ntool text")
        self.assertEqual(batch_analyze.extract_text(content, include_tool_results=False), "answer")

    def test_conversion_retains_valid_messages_calls_and_reasoning(self) -> None:
        path = self.write_session(
            "mixed.jsonl",
            [
                {
                    "type": "user",
                    "message": {
                        "content": [
                            {"type": "text", "text": None},
                            {"type": "text", "text": "inspect"},
                        ]
                    },
                },
                {
                    "type": "assistant",
                    "message": {
                        "content": [
                            {"type": "thinking", "thinking": None},
                            {"type": "thinking", "thinking": "check first"},
                            {"type": "tool_use", "id": "call-1", "name": "read", "input": {}},
                        ]
                    },
                },
                {
                    "type": "user",
                    "message": {
                        "content": [
                            {
                                "type": "tool_result",
                                "tool_use_id": "call-1",
                                "content": [
                                    {"type": "text", "text": None},
                                    {"type": "text", "text": "result"},
                                ],
                            }
                        ]
                    },
                },
            ],
        )
        converted = batch_analyze.jsonl_to_atif(path)
        self.assertEqual([step["source"] for step in converted["steps"]], ["user", "agent"])
        self.assertEqual(converted["steps"][0]["message"], "inspect")
        self.assertEqual(converted["steps"][1]["reasoning_content"], "check first")
        self.assertEqual(converted["steps"][1]["observation"]["results"][0]["content"], "result")

    @unittest.skipUnless(os.name == "posix", "fixture executable uses a POSIX shebang")
    def test_real_batch_cli_continues_to_later_valid_session(self) -> None:
        malformed = self.write_session(
            "first.jsonl",
            [
                {
                    "type": "user",
                    "message": {
                        "content": [
                            {"type": "text", "text": None},
                            {"type": "text", "text": "usable " * 300},
                        ]
                    },
                }
            ],
        )
        valid = self.write_session(
            "later.jsonl", [{"type": "user", "message": {"content": "valid " * 300}}]
        )
        os.utime(malformed, (200, 200))
        os.utime(valid, (100, 100))
        executable = self.root / "analyzer"
        executable.write_text(
            f"#!{sys.executable}\nimport json, sys\nfrom pathlib import Path\na=json.loads(Path(sys.argv[1]).read_text(encoding='utf-8'))\nprint(json.dumps({{'message': a['steps'][0]['message']}}))\n",
            encoding="utf-8",
        )
        executable.chmod(0o700)
        result = subprocess.run(
            [
                sys.executable,
                str(batch_analyze.__file__),
                "--dir",
                str(self.root),
                "--dim",
                "perf",
                "--min-steps",
                "1",
                "--binary",
                str(executable),
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('"_session_id": "first"', result.stdout)
        self.assertIn('"_session_id": "later"', result.stdout)
        self.assertIn("Done: 2 analyzed, 0 skipped, 0 failed", result.stderr)

    def test_invalid_only_messages_do_not_create_empty_steps(self) -> None:
        path = self.write_session(
            "empty.jsonl",
            [
                {
                    "type": "assistant",
                    "message": {
                        "content": [
                            {"type": "text", "text": []},
                            {"type": "thinking", "thinking": None},
                        ]
                    },
                }
            ],
        )
        self.assertIsNone(batch_analyze.jsonl_to_atif(path))

    def test_valid_text_and_truncation_remain_unchanged(self) -> None:
        self.assertEqual(
            batch_analyze.extract_text(["one", {"type": "text", "text": "two"}]), "one\ntwo"
        )
        path = self.write_session(
            "long.jsonl", [{"type": "user", "message": {"content": "x" * 6000}}]
        )
        self.assertEqual(batch_analyze.jsonl_to_atif(path)["steps"][0]["message"], "x" * 5000)

    def test_non_text_top_level_shapes_remain_empty(self) -> None:
        for value in (None, 1, {}, True):
            self.assertEqual(batch_analyze.extract_text(value), "")


if __name__ == "__main__":
    unittest.main()
