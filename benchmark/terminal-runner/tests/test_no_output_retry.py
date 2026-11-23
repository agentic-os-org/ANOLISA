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

"""Verify the no-output retry with a local subprocess and a Harbor stub.

No model service or Docker is involved.
"""

import asyncio
import importlib.util
import json
import logging
import sys
import types
from pathlib import Path
from typing import Any

import pytest


@pytest.fixture
def runner(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> Any:
    class BaseAgent:
        def __init__(self, *args: Any, **kwargs: Any) -> None:
            self.model_name = None
            self.logger = logging.getLogger("fixture-agent")

    names = (
        "harbor",
        "harbor.agents",
        "harbor.agents.base",
        "harbor.environments",
        "harbor.environments.base",
        "harbor.models",
        "harbor.models.agent",
        "harbor.models.agent.context",
    )
    for name in names:
        monkeypatch.setitem(sys.modules, name, types.ModuleType(name))
    sys.modules["harbor.agents.base"].BaseAgent = BaseAgent
    sys.modules["harbor.environments.base"].BaseEnvironment = object
    sys.modules["harbor.models.agent.context"].AgentContext = object
    path = Path(__file__).parents[1] / "external_agent" / "openclaw_external_agent.py"
    spec = importlib.util.spec_from_file_location("fixture_openclaw", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("OPENCLAW_NO_OUTPUT_TIMEOUT", "1")
    monkeypatch.setenv("OPENCLAW_TIMEOUT", "6")
    monkeypatch.setenv("OPENCLAW_MAX_ITERATIONS", "3")
    monkeypatch.setenv("OPENCLAW_AGENT_ID", "main")
    monkeypatch.setenv("OPENCLAW_MAX_STDOUT_BYTES", "102400")
    return module


@pytest.mark.parametrize("stderr", ["", "booting"])
def test_no_output_guard_returns_its_reason_and_preserves_stderr(runner: Any, stderr: str) -> None:
    code = f"import sys,time; sys.stderr.write({stderr!r}); sys.stderr.flush(); time.sleep(3)"
    returncode, stdout, error = runner.OpenClawExternalAgent._sync_run_openclaw(
        [sys.executable, "-c", code]
    )
    assert returncode != 0
    assert stdout == ""
    assert "no stdout for" in error and "likely API hang" in error
    assert error.startswith(stderr)


def _reply(text: str) -> tuple[int, str, str]:
    return (
        0,
        json.dumps(
            {
                "meta": {"agentMeta": {"sessionId": "fixture-session"}},
                "payloads": [{"text": text}],
            }
        ),
        "",
    )


def _run_loop(
    runner: Any, replies: list[tuple[int, str, str]]
) -> tuple[dict[str, Any], list[list[str]]]:
    agent = runner.OpenClawExternalAgent()
    agent._profile_name = "fixture"
    agent._harbor_container_id = "fixture-container"
    commands = []

    def run(cmd: list[str], extra_env: Any = None) -> tuple[int, str, str]:
        commands.append(list(cmd))
        assert replies, "unexpected extra OpenClaw invocation"
        return replies.pop(0)

    async def execute(environment: Any, command: str) -> Any:
        return types.SimpleNamespace(exit_code=0, stdout="done", stderr="")

    agent._sync_run_openclaw = run
    agent._env_exec = execute
    result = asyncio.run(agent._run_agent_loop("fixture task", object()))
    return result, commands


def test_successful_retry_establishes_the_session_for_the_next_turn(runner: Any) -> None:
    failure = (-9, "", "no stdout for 1s (likely API hang)")
    result, commands = _run_loop(
        runner,
        [
            failure,
            _reply("```bash\nprintf done\n```"),
            _reply("TASK_COMPLETE"),
        ],
    )
    assert result["returncode"] == 0
    assert len(commands) == 3
    assert commands[0] == commands[1]
    assert commands[2][commands[2].index("--session-id") + 1] == "fixture-session"
    assert len(result["harbor_executions"]) == 1


def test_real_no_output_failure_reaches_the_existing_retry_policy(runner: Any) -> None:
    failure = runner.OpenClawExternalAgent._sync_run_openclaw(
        [sys.executable, "-c", "import time; time.sleep(3)"]
    )
    result, commands = _run_loop(
        runner, [failure, _reply("```bash\nprintf done\n```"), _reply("TASK_COMPLETE")]
    )
    assert result["returncode"] == 0
    assert len(commands) == 3
    assert commands[2][commands[2].index("--session-id") + 1] == "fixture-session"


def test_initial_success_preserves_the_session(runner: Any) -> None:
    result, commands = _run_loop(
        runner,
        [
            _reply("```bash\nprintf done\n```"),
            _reply("TASK_COMPLETE"),
        ],
    )
    assert result["returncode"] == 0
    assert len(commands) == 2
    assert commands[1][commands[1].index("--session-id") + 1] == "fixture-session"


def test_repeated_hangs_stop_after_exactly_one_retry(runner: Any) -> None:
    failure = (-9, "", "no stdout for 1s (likely API hang)")
    result, commands = _run_loop(runner, [failure, failure])
    assert result["returncode"] == -9
    assert len(commands) == 2


def test_ordinary_failure_is_not_retried(runner: Any) -> None:
    result, commands = _run_loop(runner, [(2, "", "ordinary failure")])
    assert result["returncode"] == 2
    assert len(commands) == 1
