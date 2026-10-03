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

"""Regression test: _collect_session_tokens must not sum cumulative inputs.

OpenClaw session usage reports `input` as the CUMULATIVE context size at
each assistant turn (the code's own input_delta logic relies on it).
Summing those per-round cumulative values inflated total_input (and
total_tokens, and the per-round "cumulative" field) superlinearly with
conversation length in AgentContext.metadata["session_tokens"].
"""

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

MODULE = Path(__file__).resolve().parent / "openclaw_external_agent.py"


def _install_harbor_stubs(stub_root: Path):
    """Minimal harbor stubs so the module imports without the real package."""
    pkg = {
        "harbor/__init__.py": "",
        "harbor/agents/__init__.py": "",
        "harbor/agents/base.py": (
            "class BaseAgent:\n"
            "    def __init__(self, *args, **kwargs):\n"
            "        import logging\n"
            "        self.logger = logging.getLogger('stub')\n"
            "        self.model_name = kwargs.get('model_name', '')\n"
        ),
        "harbor/environments/__init__.py": "",
        "harbor/environments/base.py": "class BaseEnvironment:\n    pass\n",
        "harbor/models/__init__.py": "",
        "harbor/models/agent/__init__.py": "",
        "harbor/models/agent/context.py": "class AgentContext:\n    pass\n",
    }
    for rel, src in pkg.items():
        p = stub_root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(src, encoding="utf-8")


def _load_agent_module():
    with tempfile.TemporaryDirectory() as tmp:
        stub_root = Path(tmp)
        _install_harbor_stubs(stub_root)
        sys.path.insert(0, str(stub_root))
        try:
            spec = importlib.util.spec_from_file_location(
                "openclaw_external_agent_under_test", MODULE)
            mod = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(mod)
            return mod
        finally:
            sys.path.remove(str(stub_root))


class SessionTokenTotalsTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.profile = Path(self._tmp.name)
        self.agent = _load_agent_module().OpenClawExternalAgent()

    def tearDown(self):
        self._tmp.cleanup()

    def _write_session(self, rounds):
        sessions = self.profile / "agents" / "main" / "sessions"
        sessions.mkdir(parents=True, exist_ok=True)
        with open(sessions / "s1.jsonl", "w") as f:
            for inp, out in rounds:
                f.write(json.dumps({
                    "type": "message",
                    "message": {
                        "role": "assistant",
                        "usage": {"input": inp, "output": out,
                                  "totalTokens": inp + out},
                    },
                }) + "\n")

    def test_totals_use_final_cumulative_input(self):
        """100/200/300 cumulative inputs -> total_input 300, not 600."""
        self._write_session([(100, 5), (200, 7), (300, 9)])
        rep = self.agent._collect_session_tokens(str(self.profile), "main")
        self.assertEqual(rep["num_rounds"], 3)
        self.assertEqual(rep["total_input"], 300)
        self.assertEqual(rep["total_output"], 21)
        self.assertEqual(rep["total_tokens"], 321)
        self.assertEqual(rep["final_input"], 300)
        self.assertEqual(rep["avg_input_delta"], 100)

    def test_round_cumulative_is_monotonic(self):
        """Per-round cumulative = cumulative input so far + outputs so far."""
        self._write_session([(100, 5), (200, 7), (300, 9)])
        rep = self.agent._collect_session_tokens(str(self.profile), "main")
        cumulative = [r["cumulative"] for r in rep["rounds"]]
        self.assertEqual(cumulative, [105, 212, 321])
        self.assertEqual(cumulative[-1], rep["total_tokens"])

    def test_inflation_does_not_grow_with_rounds(self):
        """Longer sessions no longer report superlinear total_input."""
        self._write_session([(100 * (i + 1), 1) for i in range(10)])
        rep = self.agent._collect_session_tokens(str(self.profile), "main")
        self.assertEqual(rep["total_input"], 1000)   # final cumulative
        self.assertLess(rep["total_input"], 5500)    # naive sum = 5500

    def test_missing_session_returns_none(self):
        self.assertIsNone(
            self.agent._collect_session_tokens(str(self.profile), "main"))


if __name__ == "__main__":
    unittest.main()
