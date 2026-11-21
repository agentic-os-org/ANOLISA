# Copyright 2026 Alibaba Cloud
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

"""Openclaw session and ce-runner trace JSONL readers decode UTF-8.

``session_trace_converter.py`` reads the openclaw session format and writes
the ce-runner trace format explicitly as UTF-8 (issue #10399 cites that
convention).  The remaining readers of these two JSONL formats used to open
their files with the process default encoding, so under a legacy ANSI code
page (for example ``cp936`` on a zh-CN Windows host) a valid UTF-8 final
reply arrived as mojibake or raised ``UnicodeDecodeError`` that the readers
swallow into an empty result.

The real reader definitions are loaded from the merged source files with
``ast`` inside isolated interpreter subprocesses -- no ce_runner import, no
gateway, services or model -- mirroring ``test_session_trace_utf8.py``.
"""

import ast
import codecs
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
PROMPT_TASK = REPO_ROOT / "scripts" / "prompt_task.py"
AGENT = REPO_ROOT / "src" / "ce_runner" / "agent.py"
PIPELINE = REPO_ROOT / "src" / "ce_runner" / "pipeline.py"

USER_TEXT = "\u8bf7\u7edf\u8ba1\u90ae\u7bb1\u6570\u91cf \U0001f4ec"
FINAL_TEXT = "\u5df2\u5168\u90e8\u7edf\u8ba1\u5b8c\u6210 caf\u00e9 \u2705"

EXPECTED_SCORES = {
    "completion": 1.0, "robustness": 0.5, "communication": 1.0,
    "safety": 1.0, "task_score": 4.5, "passed": True,
}

CHILD_RUNNER = r'''
import ast
import json
import sys
from pathlib import Path

source_path = Path(sys.argv[1])
scenario = sys.argv[2]
session_file = sys.argv[3] if len(sys.argv) > 3 else ""
trace_file = sys.argv[4] if len(sys.argv) > 4 else ""

wanted = {
    "parse_final_reply": ["_parse_assistant_response"],
    "has_tool_calls": ["_get_last_assistant_has_tool_calls"],
    "conversation": ["_build_conversation_for_user_agent"],
    "grading_scores": ["_read_grading_scores"],
}[scenario]
source = ast.parse(source_path.read_text(encoding="utf-8"))
body = [node for node in source.body
        if isinstance(node, ast.FunctionDef) and node.name in wanted]
assert len(body) == len(wanted), \
    "definitions missing from {}: {}".format(source_path, sorted(wanted))
scope = {"json": json, "log": lambda *args, **kwargs: None}
exec(compile(ast.Module(body=body, type_ignores=[]), str(source_path), "exec"),
     scope)

if scenario == "parse_final_reply":
    result = scope["_parse_assistant_response"](session_file)
elif scenario == "has_tool_calls":
    result = scope["_get_last_assistant_has_tool_calls"](session_file)
elif scenario == "conversation":
    result = scope["_build_conversation_for_user_agent"](session_file)
else:
    result = scope["_read_grading_scores"](trace_file)

# ensure_ascii keeps the child stdout encoding out of the contract.
print(json.dumps({"result": result}, ensure_ascii=True))
'''.strip() + "\n"


class ReaderUtf8TestCase(unittest.TestCase):
    """Shared fixture/environment plumbing for the reader sources."""

    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="ce-reader-encoding-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.session = self.root / "session.jsonl"
        self.trace = self.root / "trace.jsonl"

    # -- fixtures ------------------------------------------------------------

    def write_session(self, *, escaped: bool,
                      with_tool_calls: bool = False) -> None:
        events = [
            {
                "type": "message",
                "message": {
                    "role": "user",
                    "content": [{"type": "text", "text": USER_TEXT}],
                },
            },
            {
                "type": "message",
                "message": {
                    "role": "assistant",
                    "content": (
                        [
                            {"type": "toolCall", "id": "call_1",
                             "function": {"name": "mail_search",
                                          "arguments": {"query": USER_TEXT}}},
                            {"type": "text", "text": FINAL_TEXT},
                        ]
                        if with_tool_calls
                        else [{"type": "text", "text": FINAL_TEXT}]
                    ),
                },
            },
        ]
        self.session.write_text(
            "".join(json.dumps(event, ensure_ascii=escaped) + "\n"
                    for event in events),
            encoding="utf-8",
        )

    def write_trace(self) -> None:
        events = [
            {"type": "trace_start", "task_id": "T001zh_encoding"},
            {
                "type": "message",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": FINAL_TEXT}],
                },
            },
            {
                "type": "grading_result",
                "scores": {"completion": 1.0, "robustness": 0.5,
                           "communication": 1.0, "safety": 1.0},
                "task_score": 4.5,
                "passed": True,
            },
            {"type": "trace_end"},
        ]
        self.trace.write_text(
            "".join(json.dumps(event, ensure_ascii=False) + "\n"
                    for event in events),
            encoding="utf-8",
        )

    # -- child plumbing --------------------------------------------------------

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

    def run_reader(self, source: Path, scenario: str, *,
                   session_file: Path | None = None,
                   trace_file: Path | None = None,
                   utf8_mode: bool = False):
        runner = self.root / ("runner_%s.py" % scenario)
        runner.write_text(CHILD_RUNNER, encoding="utf-8")
        result = subprocess.run(
            [
                sys.executable,
                "-X",
                "utf8=%d" % int(utf8_mode),
                str(runner),
                str(source),
                scenario,
                str(session_file or self.session),
                str(trace_file or self.trace),
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
            result.stderr.decode("utf-8", errors="replace"),
        )
        return json.loads(result.stdout.decode("utf-8"))["result"]


class TestParseAssistantResponseUtf8(ReaderUtf8TestCase):
    """scripts/prompt_task.py -- the final-reply reader from issue #10399."""

    def test_literal_utf8_final_reply_under_legacy_file_encoding(self) -> None:
        self.require_legacy_file_encoding()
        self.write_session(escaped=False)
        self.assertEqual(
            self.run_reader(PROMPT_TASK, "parse_final_reply", utf8_mode=False),
            FINAL_TEXT,
        )

    def test_ascii_escaped_final_reply_under_legacy_file_encoding(self) -> None:
        self.require_legacy_file_encoding()
        self.write_session(escaped=True)
        self.assertEqual(
            self.run_reader(PROMPT_TASK, "parse_final_reply", utf8_mode=False),
            FINAL_TEXT,
        )

    def test_literal_utf8_final_reply_under_utf8_mode(self) -> None:
        self.write_session(escaped=False)
        self.assertEqual(
            self.run_reader(PROMPT_TASK, "parse_final_reply", utf8_mode=True),
            FINAL_TEXT,
        )


class TestAgentSessionReadersUtf8(ReaderUtf8TestCase):
    """src/ce_runner/agent.py -- the in-runner session readers."""

    def test_literal_utf8_tool_call_detection_under_legacy_file_encoding(self) -> None:
        self.require_legacy_file_encoding()
        self.write_session(escaped=False, with_tool_calls=True)
        self.assertTrue(
            self.run_reader(AGENT, "has_tool_calls", utf8_mode=False)
        )

    def test_literal_utf8_conversation_under_legacy_file_encoding(self) -> None:
        self.require_legacy_file_encoding()
        self.write_session(escaped=False)
        self.assertEqual(
            self.run_reader(AGENT, "conversation", utf8_mode=False),
            [
                {"role": "user", "text": USER_TEXT},
                {"role": "assistant", "text": FINAL_TEXT},
            ],
        )

    def test_ascii_escaped_conversation_under_legacy_file_encoding(self) -> None:
        self.require_legacy_file_encoding()
        self.write_session(escaped=True)
        self.assertEqual(
            self.run_reader(AGENT, "conversation", utf8_mode=False),
            [
                {"role": "user", "text": USER_TEXT},
                {"role": "assistant", "text": FINAL_TEXT},
            ],
        )

    def test_literal_utf8_conversation_under_utf8_mode(self) -> None:
        self.write_session(escaped=False)
        self.assertEqual(
            self.run_reader(AGENT, "conversation", utf8_mode=True),
            [
                {"role": "user", "text": USER_TEXT},
                {"role": "assistant", "text": FINAL_TEXT},
            ],
        )


class TestPipelineGradingScoresUtf8(ReaderUtf8TestCase):
    """src/ce_runner/pipeline.py -- the grading-result trace reader."""

    def test_literal_utf8_trace_under_legacy_file_encoding(self) -> None:
        self.require_legacy_file_encoding()
        self.write_trace()
        scores, seen = self.run_reader(
            PIPELINE, "grading_scores", utf8_mode=False,
        )
        self.assertTrue(seen)
        self.assertEqual(scores, EXPECTED_SCORES)

    def test_literal_utf8_trace_under_utf8_mode(self) -> None:
        self.write_trace()
        scores, seen = self.run_reader(
            PIPELINE, "grading_scores", utf8_mode=True,
        )
        self.assertTrue(seen)
        self.assertEqual(scores, EXPECTED_SCORES)

    def test_trace_without_grading_result_reports_unseen(self) -> None:
        self.trace.write_text(
            json.dumps({"type": "trace_end"}) + "\n", encoding="utf-8"
        )
        scores, seen = self.run_reader(
            PIPELINE, "grading_scores", utf8_mode=True,
        )
        self.assertFalse(seen)
        self.assertFalse(scores["passed"])
        self.assertEqual(scores["task_score"], 0.0)


if __name__ == "__main__":
    unittest.main()
