"""Offline checks for the installed-package probe's host event parsing."""

from __future__ import annotations

import runpy
import unittest
from pathlib import Path
from unittest.mock import patch

with patch.object(Path, "read_text", return_value="{}"):
    tool_events = runpy.run_path(str(Path(__file__).parent / "release_regression" / "probe.py"))[
        "tool_events"
    ]


class ReleaseRegressionProbeTests(unittest.TestCase):
    def test_claude_tool_result_text_forms_preserve_bytes(self) -> None:
        text = "omitted test\nUnicode: 中文\n"
        for content in (
            text,
            [
                {"type": "text", "text": "omitted test\n"},
                {"type": "image", "source": {}},
                {"type": "text", "text": "Unicode: 中文\n"},
            ],
        ):
            with self.subTest(content=content):
                calls, results, final, usage = tool_events(
                    "claude-code",
                    [
                        {
                            "message": {
                                "content": [
                                    {
                                        "type": "tool_use",
                                        "id": "retrieve-1",
                                        "input": {"command": "tokenless retrieve HASH"},
                                    },
                                    {
                                        "type": "tool_result",
                                        "tool_use_id": "retrieve-1",
                                        "content": content,
                                    },
                                ]
                            }
                        },
                        {"type": "result", "result": "answer", "usage": {"output_tokens": 5}},
                    ],
                )
                self.assertEqual(calls, {"retrieve-1": "tokenless retrieve HASH"})
                self.assertEqual(results, {"retrieve-1": text})
                self.assertEqual(final, "answer")
                self.assertEqual(usage, {"output_tokens": 5})


if __name__ == "__main__":
    unittest.main()
