#!/usr/bin/env python3

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

"""Regression test: LLM health counters in generate_trial_reports.py.

The ok/error check counted a failed trial's classification as ok whenever
key_reason_zh was present - but llm_classify_failure's exception path
ALWAYS sets key_reason_zh ("LLM error: ..."), so a hard 401 printed
"1 ok, 0 errors". Errors must be detected via the sentinel prefix.
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "generate_trial_reports.py"

_FAIL_TRACE = "\n".join([
    json.dumps({"type": "grading_result", "task_id": "T001", "passed": False,
                "task_score": 0.1, "scores": {"completion": 0.1},
                "judge_calls": [{"score": 0.0, "rubric_preview": "r",
                                 "reasoning": "x"}]}),
    json.dumps({"type": "trace_end", "total_turns": 2, "wall_time_s": 5}),
]) + "\n"

_STUB_RAISING = '''
class _Completions:
    @staticmethod
    def create(**k):
        raise RuntimeError("401 invalid api key")
class _Chat:
    completions = _Completions()
class OpenAI:
    def __init__(self, *a, **k):
        self.chat = _Chat()
'''

_STUB_CONTENT_TMPL = '''
class _NS:
    def __init__(self, **kw):
        self.__dict__.update(kw)
class _Completions:
    @staticmethod
    def create(**k):
        msg = _NS(content={content!r})
        choice = _NS(message=msg)
        return _NS(choices=[choice])
class _Chat:
    completions = _Completions()
class OpenAI:
    def __init__(self, *a, **k):
        self.chat = _Chat()
'''


class LlmHealthCounterTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        base = Path(self._tmp.name)
        self.trace_dir = base / "traces"
        self.output_dir = base / "reports"
        self.stub_dir = base / "stubs"
        self.trace_dir.mkdir(parents=True)
        (self.trace_dir / "T001_err99.jsonl").write_text(
            _FAIL_TRACE, encoding="utf-8")

    def tearDown(self):
        self._tmp.cleanup()

    def _install_stub(self, src: str):
        pkg = self.stub_dir / "openai"
        pkg.mkdir(parents=True, exist_ok=True)
        (pkg / "__init__.py").write_text(src, encoding="utf-8")

    def _run(self) -> str:
        env = os.environ.copy()
        env["PYTHONPATH"] = str(self.stub_dir)
        result = subprocess.run(
            [sys.executable, str(SCRIPT),
             "--trace-dir", str(self.trace_dir),
             "--tasks-dir", "/nonexistent",
             "--output-dir", str(self.output_dir),
             "--judge-api-key", "sk-bad",
             "--judge-model", "m1"],
            capture_output=True, text=True, timeout=60, env=env,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def test_hard_llm_failure_counts_as_error(self):
        """Stubbed 401 -> 'LLM classification: 0 ok, 1 errors'."""
        self._install_stub(_STUB_RAISING)
        stdout = self._run()
        self.assertIn("LLM classification: 0 ok, 1 errors", stdout)
        report = json.loads(
            (self.output_dir / "T001_err99.json").read_text(encoding="utf-8"))
        self.assertTrue(report["failure_classification"]["key_reason_zh"]
                        .startswith("LLM error"))

    def test_real_classification_counts_as_ok(self):
        """A successful classification -> '1 ok, 0 errors'."""
        self._install_stub(_STUB_CONTENT_TMPL.format(
            content='{"category": "tool_error", "key_reason_zh": "工具调用失败"}'))
        stdout = self._run()
        self.assertIn("LLM classification: 1 ok, 0 errors", stdout)

    def test_legitimate_other_category_still_ok(self):
        """LLM legitimately answering category 'other' is NOT an error."""
        self._install_stub(_STUB_CONTENT_TMPL.format(
            content='{"category": "other", "key_reason_zh": "任务超时"}'))
        stdout = self._run()
        self.assertIn("LLM classification: 1 ok, 0 errors", stdout)


if __name__ == "__main__":
    unittest.main()
