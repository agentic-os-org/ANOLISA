"""The ATIF file handed to analysis must be UTF-8 independently of locale."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from test_batch_analyze import batch_analyze


@unittest.skipUnless(os.name == "posix", "the forced C locale fixture requires POSIX")
class BatchAtifEncodingTests(unittest.TestCase):
    def convert_through_cli(self, event: dict, utf8_mode: str = "0") -> dict:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            session = root / "session.jsonl"
            session.write_text(json.dumps(event, ensure_ascii=False) + "\n", encoding="utf-8")
            analyzer = root / "analyzer.py"
            analyzer.write_text(
                f"#!{sys.executable}\nimport json,sys\nfrom pathlib import Path\n"
                "doc=json.loads(Path(sys.argv[1]).read_text(encoding='utf-8'))\n"
                "print(json.dumps({'document':json.dumps(doc)}))\n",
                encoding="utf-8",
            )
            analyzer.chmod(0o700)
            result = subprocess.run(
                [
                    sys.executable,
                    batch_analyze.__file__,
                    "--file",
                    str(session),
                    "--dim",
                    "perf",
                    "--min-steps",
                    "1",
                    "--binary",
                    str(analyzer),
                ],
                env={
                    **os.environ,
                    "LC_ALL": "C",
                    "PYTHONCOERCECLOCALE": "0",
                    "PYTHONUTF8": utf8_mode,
                    "PYTHONIOENCODING": "utf-8",
                    "TMPDIR": str(root),
                },
                capture_output=True,
                text=True,
                encoding="utf-8",
                timeout=20,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("Done: 1 analyzed, 0 skipped, 0 failed", result.stderr)
            self.assertFalse(list(root.glob("atif-*.json")))
            return json.loads(json.loads(result.stdout)["document"])

    def test_plain_ascii_control(self) -> None:
        document = self.convert_through_cli(
            {"type": "user", "message": {"role": "user", "content": "Inspect the project."}}
        )
        self.assertEqual(document["steps"][0]["message"], "Inspect the project.")

    def test_chinese_text_survives_a_non_utf8_locale(self) -> None:
        text = "\u8bf7\u68c0\u67e5\u8fd9\u4e2a\u9879\u76ee"
        document = self.convert_through_cli(
            {"type": "user", "message": {"role": "user", "content": text}}
        )
        self.assertEqual(document["steps"][0]["message"], text)

    def test_emoji_survives_a_non_utf8_locale(self) -> None:
        text = "Inspect \U0001f680 and \U0001f4c1"
        document = self.convert_through_cli(
            {"type": "user", "message": {"role": "user", "content": text}}
        )
        self.assertEqual(document["steps"][0]["message"], text)

    def test_unicode_tool_arguments_and_reasoning_reach_analysis(self) -> None:
        path = "\u9879\u76ee/\u8bf4\u660e.txt"
        reasoning = "Read the \U0001f4c1 first."
        document = self.convert_through_cli(
            {
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": reasoning},
                        {
                            "type": "tool_use",
                            "id": "a",
                            "name": "Read",
                            "input": {"file_path": path},
                        },
                    ],
                },
            }
        )
        self.assertEqual(document["steps"][0]["tool_calls"][0]["arguments"]["file_path"], path)
        self.assertEqual(document["steps"][0]["reasoning_content"], reasoning)

    def test_utf8_mode_control_still_works(self) -> None:
        text = "\u4e2d\u6587 \U0001f680"
        document = self.convert_through_cli(
            {"type": "user", "message": {"role": "user", "content": text}}, utf8_mode="1"
        )
        self.assertEqual(document["steps"][0]["message"], text)


if __name__ == "__main__":
    unittest.main()
