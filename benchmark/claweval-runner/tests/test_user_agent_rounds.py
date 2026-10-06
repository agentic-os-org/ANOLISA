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

"""Test the UserAgent multi-round dialogue loop and its LLM retries.

Covers the interaction between run_agent, the UserAgent LLM reply and the
_run_agent_continue step, without spawning any openclaw CLI process.
"""

import sys
import types
from pathlib import Path
from unittest.mock import patch

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


def _c_task(tmp_path, max_rounds: int = 3) -> str:
    task_yaml = tmp_path / "task.yaml"
    task_yaml.write_text(
        "task_id: C01zh_mortgage\n"
        "prompt: 你好\n"
        f"user_agent:\n"
        f"  enabled: true\n"
        f"  max_rounds: {max_rounds}\n"
        f"  persona: 测试人设\n"
    )
    return str(task_yaml)


class TestUserAgentRounds:
    """Test run_agent_with_user_agent round handling."""

    def test_failed_continue_breaks_the_loop(self, tmp_path):
        """A failed continue round ends the loop (kept minimal here; the
        last-good-session return value is covered by a separate change)."""
        from ce_runner import agent as agent_mod

        task_yaml = _c_task(tmp_path, max_rounds=3)
        good_session = str(tmp_path / "sess-round1.jsonl")
        Path(good_session).write_text("{}\n")

        replies = iter(["再问一下利率", "还有手续费吗"])

        with patch.object(agent_mod, "run_agent",
                          return_value=good_session), \
             patch.object(agent_mod, "_get_last_assistant_has_tool_calls",
                          return_value=False), \
             patch.object(agent_mod, "_call_user_agent_llm",
                          side_effect=lambda *a, **k: next(replies)), \
             patch.object(agent_mod, "_run_agent_continue",
                          return_value="") as mock_cont:
            agent_mod.run_agent_with_user_agent(
                "sess-1", task_yaml, 30, {"api_key": "k"}, agent_id="a1")

        assert mock_cont.call_count == 1, (
            "a failed continue must stop the round loop, not keep looping"
        )

    def test_loop_stops_when_user_satisfied(self, tmp_path):
        """UserAgent [DONE] (None reply) ends the loop after round 1."""
        from ce_runner import agent as agent_mod

        task_yaml = _c_task(tmp_path, max_rounds=5)
        good_session = str(tmp_path / "sess.jsonl")
        Path(good_session).write_text("{}\n")

        with patch.object(agent_mod, "run_agent",
                          return_value=good_session), \
             patch.object(agent_mod, "_get_last_assistant_has_tool_calls",
                          return_value=False), \
             patch.object(agent_mod, "_call_user_agent_llm",
                          return_value=None) as mock_ua, \
             patch.object(agent_mod, "_run_agent_continue") as mock_cont:
            result = agent_mod.run_agent_with_user_agent(
                "sess-1", task_yaml, 30, {"api_key": "k"}, agent_id="a1")

        assert result == good_session
        assert mock_ua.call_count == 1
        assert not mock_cont.called


class _FakeOpenAIError(Exception):
    """Stand-in for openai's APIStatusError shape (has status_code)."""

    def __init__(self, status_code: int):
        super().__init__(f"HTTP {status_code}")
        self.status_code = status_code


class TestUserAgentLlmRetries:
    """_call_user_agent_llm must not retry permanent API rejections."""

    def _install_openai_stub(self, monkeypatch, exc: Exception):
        calls = {"count": 0}

        class _Completions:
            def create(self, **kwargs):
                calls["count"] += 1
                raise exc

        class _Client:
            def __init__(self, **kwargs):
                self.chat = types.SimpleNamespace(completions=_Completions())

        openai_stub = types.ModuleType("openai")
        openai_stub.OpenAI = _Client
        monkeypatch.setitem(sys.modules, "openai", openai_stub)
        return calls

    def test_auth_error_is_not_retried(self, tmp_path, monkeypatch):
        """A 401 rejection is permanent; retrying burns ~2 minutes per round."""
        from ce_runner import agent as agent_mod

        calls = self._install_openai_stub(monkeypatch, _FakeOpenAIError(401))
        sleeps: list[float] = []
        monkeypatch.setattr(agent_mod.time, "sleep", lambda s: sleeps.append(s))

        result = agent_mod._call_user_agent_llm(
            {"api_key": "bad", "base_url": "http://x", "model_id": "m"},
            "人设", [{"role": "user", "text": "你好"}])

        assert result is None
        assert calls["count"] == 1, "a 401 must not be retried"
        assert sleeps == []

    def test_transient_error_is_still_retried(self, tmp_path, monkeypatch):
        """Guard: connection-ish errors (no status_code) keep the retry loop."""
        from ce_runner import agent as agent_mod

        class _Transient(Exception):
            pass

        calls = self._install_openai_stub(monkeypatch, _Transient("boom"))
        monkeypatch.setattr(agent_mod.time, "sleep", lambda s: None)

        result = agent_mod._call_user_agent_llm(
            {"api_key": "k", "base_url": "http://x", "model_id": "m"},
            "人设", [{"role": "user", "text": "你好"}])

        assert result is None
        assert calls["count"] == 10, "transient errors must keep retrying"
