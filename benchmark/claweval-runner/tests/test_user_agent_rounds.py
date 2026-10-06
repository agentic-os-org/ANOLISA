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

"""Test the UserAgent multi-round dialogue loop.

Covers the interaction between run_agent, the UserAgent LLM reply and the
_run_agent_continue step, without spawning any openclaw CLI process.
"""

import sys
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

    def test_failed_continue_keeps_last_good_session(self, tmp_path):
        """A failed continue round must not discard the earlier session.

        Rounds 1..N-1 wrote a complete session file; a transient CLI failure
        on the LAST continue used to overwrite the return value with "" so
        the caller recorded the whole run as an execution error and never
        graded the conversation that exists on disk.
        """
        from ce_runner import agent as agent_mod

        task_yaml = _c_task(tmp_path, max_rounds=3)
        good_session = str(tmp_path / "sess-round1.jsonl")
        Path(good_session).write_text("{}\n")

        replies = iter(["再问一下利率", "还有手续费吗"])

        with patch.object(agent_mod, "run_agent",
                          return_value=good_session) as mock_run, \
             patch.object(agent_mod, "_get_last_assistant_has_tool_calls",
                          return_value=False), \
             patch.object(agent_mod, "_call_user_agent_llm",
                          side_effect=lambda *a, **k: next(replies)), \
             patch.object(agent_mod, "_run_agent_continue",
                          return_value=""):
            result = agent_mod.run_agent_with_user_agent(
                "sess-1", task_yaml, 30, {"api_key": "k"}, agent_id="a1")

        assert mock_run.called
        assert result == good_session, (
            "a failed continue must keep the last known-good session file"
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

    def test_successful_continue_moves_session_forward(self, tmp_path):
        """Each successful continue round replaces the returned session."""
        from ce_runner import agent as agent_mod

        task_yaml = _c_task(tmp_path, max_rounds=2)
        round1 = str(tmp_path / "sess-round1.jsonl")
        Path(round1).write_text("{}\n")
        round2 = str(tmp_path / "sess-round2.jsonl")
        Path(round2).write_text("{}\n")

        replies = iter(["再问一下利率", None])  # one continue, then done

        with patch.object(agent_mod, "run_agent", return_value=round1), \
             patch.object(agent_mod, "_get_last_assistant_has_tool_calls",
                          return_value=False), \
             patch.object(agent_mod, "_call_user_agent_llm",
                          side_effect=lambda *a, **k: next(replies)), \
             patch.object(agent_mod, "_run_agent_continue",
                          return_value=round2) as mock_cont:
            result = agent_mod.run_agent_with_user_agent(
                "sess-1", task_yaml, 30, {"api_key": "k"}, agent_id="a1")

        assert mock_cont.call_count == 1
        assert result == round2
