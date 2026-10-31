# Copyright 2026 Hanabi21
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Run the complete converter CLI with its real dependencies and file inputs.

ANOLISA_CONVERTER_PATH selects the baseline or candidate complete source file.
No module, logger, HTTP client, YAML parser or conversion logic is replaced.
The existing --audit-data option supplies an empty saved audit, avoiding HTTP.
"""

import codecs
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

PROJECT_CONVERTER = (
    Path(__file__).resolve().parents[1] / "src" / "ce_runner" / "session_trace_converter.py"
)
CONVERTER = Path(os.environ.get("ANOLISA_CONVERTER_PATH", PROJECT_CONVERTER)).resolve()


class TestSessionTraceUtf8(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="ce-trace-encoding-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.session = self.root / "session.jsonl"
        self.output = self.root / "converted" / "trace.jsonl"
        self.task = self.root / "task.yaml"
        self.audit = self.root / "audit.json"
        self.task.write_text(
            "task_id: T001zh_encoding\nservices: []\ntools: []\n",
            encoding="ascii",
        )
        self.audit.write_text("{}\n", encoding="ascii")

    def child_environment(self, utf8_mode: bool) -> dict[str, str]:
        env = os.environ.copy()
        env.update(
            {
                "LC_ALL": "C",
                "LANG": "C",
                "PYTHONCOERCECLOCALE": "0",
                "PYTHONUTF8": "1" if utf8_mode else "0",
                # Keep console encoding separate from the file-encoding contract.
                "PYTHONIOENCODING": "utf-8",
            }
        )
        return env

    def require_legacy_file_encoding(self) -> None:
        result = subprocess.run(
            [
                sys.executable,
                "-X",
                "utf8=0",
                "-c",
                "import locale; print(locale.getencoding())",
            ],
            cwd=self.root,
            env=self.child_environment(False),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=10,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        encoding = result.stdout.decode("utf-8").strip()
        if codecs.lookup(encoding).name == "utf-8":
            self.skipTest(
                "This interpreter still has UTF-8 file encoding with UTF-8 "
                "mode disabled; a real non-UTF-8 locale is required."
            )

    def convert(self, text: str, *, escaped: bool, utf8_mode: bool) -> None:
        events = [
            {
                "type": "message",
                "timestamp": "2026-05-27T03:00:00.000Z",
                "message": {
                    "role": "user",
                    "content": [{"type": "text", "text": text}],
                },
            },
            {
                "type": "message",
                "timestamp": "2026-05-27T03:00:05.000Z",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": text}],
                    "usage": {"input": 17, "output": 5},
                    "stopReason": "endTurn",
                },
            },
        ]
        self.session.write_text(
            "".join(json.dumps(event, ensure_ascii=escaped) + "\n" for event in events),
            encoding="utf-8",
        )
        result = subprocess.run(
            [
                sys.executable,
                "-X",
                f"utf8={int(utf8_mode)}",
                str(CONVERTER),
                "--session",
                str(self.session),
                "--task-yaml",
                str(self.task),
                "--output",
                str(self.output),
                "--audit-data",
                str(self.audit),
            ],
            cwd=self.root,
            env=self.child_environment(utf8_mode),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=20,
            check=False,
        )
        self.assertEqual(
            result.returncode,
            0,
            (result.stdout + result.stderr).decode("utf-8", errors="replace"),
        )
        # This is the encoding used by the real comparison consumer.
        converted = [
            json.loads(line)
            for line in self.output.read_text(encoding="utf-8").splitlines()
            if line
        ]
        self.assertEqual(converted[0]["type"], "trace_start")
        self.assertEqual(converted[0]["task_id"], "T001zh_encoding")
        messages = [event for event in converted if event["type"] == "message"]
        self.assertEqual([event["message"]["role"] for event in messages], ["user", "assistant"])
        self.assertEqual(
            [event["message"]["content"][0]["text"] for event in messages],
            [text, text],
        )
        end = converted[-1]
        self.assertEqual(end["type"], "trace_end")
        self.assertEqual(end["model_input_tokens"], 17)
        self.assertEqual(end["model_output_tokens"], 5)
        self.assertEqual(end["total_tokens"], 22)
        self.assertEqual(end["final_text"], text)
        self.assertEqual(json.loads(result.stdout)["input_tokens"], 17)

    def test_literal_utf8_session_under_legacy_file_encoding(self) -> None:
        self.require_legacy_file_encoding()
        self.convert("中文任务 café 🦊", escaped=False, utf8_mode=False)

    def test_ascii_escaped_session_writes_utf8_under_legacy_file_encoding(self) -> None:
        self.require_legacy_file_encoding()
        self.convert("中文任务 café 🦊", escaped=True, utf8_mode=False)

    def test_ascii_session_control(self) -> None:
        self.convert("ordinary task text", escaped=True, utf8_mode=False)

    def test_existing_utf8_mode_unicode_control(self) -> None:
        self.convert("中文任务 café 🦊", escaped=False, utf8_mode=True)


if __name__ == "__main__":
    unittest.main()
