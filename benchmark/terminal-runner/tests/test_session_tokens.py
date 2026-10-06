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

"""Session token totals must not sum cumulative inputs.

OpenClaw session JSONL reports each assistant round's ``usage.input`` as
the full (cumulative) prompt for that round — the collector already
derives ``input_delta`` from consecutive values but then added the raw
cumulative ``inp`` into ``total_input`` on every round, so totals (and the
per-round ``cumulative`` field) grew quadratically with conversation
length.
"""

import importlib.util
import json
import logging
import sys
import types
from pathlib import Path

AGENT_FILE = (
    Path(__file__).resolve().parents[1]
    / "external_agent"
    / "openclaw_external_agent.py"
)


def _install_harbor_stubs():
    if "harbor.agents.base" in sys.modules:
        return

    class BaseAgent:
        def __init__(self, *args, **kwargs):
            self.logger = logging.getLogger("test-agent")
            self.model_name = ""

    class BaseEnvironment:  # noqa: D401 - minimal stub
        pass

    class AgentContext:  # noqa: D401 - minimal stub
        pass

    modules = {
        "harbor": types.ModuleType("harbor"),
        "harbor.agents": types.ModuleType("harbor.agents"),
        "harbor.agents.base": types.ModuleType("harbor.agents.base"),
        "harbor.environments": types.ModuleType("harbor.environments"),
        "harbor.environments.base": types.ModuleType("harbor.environments.base"),
        "harbor.models": types.ModuleType("harbor.models"),
        "harbor.models.agent": types.ModuleType("harbor.models.agent"),
        "harbor.models.agent.context": types.ModuleType("harbor.models.agent.context"),
    }
    for name in ("harbor", "harbor.agents", "harbor.environments", "harbor.models", "harbor.models.agent"):
        modules[name].__path__ = []
    modules["harbor.agents.base"].BaseAgent = BaseAgent
    modules["harbor.environments.base"].BaseEnvironment = BaseEnvironment
    modules["harbor.models.agent.context"].AgentContext = AgentContext
    modules["harbor"].agents = modules["harbor.agents"]
    modules["harbor.agents"].base = modules["harbor.agents.base"]
    modules["harbor"].environments = modules["harbor.environments"]
    modules["harbor.environments"].base = modules["harbor.environments.base"]
    modules["harbor"].models = modules["harbor.models"]
    modules["harbor.models"].agent = modules["harbor.models.agent"]
    modules["harbor.models.agent"].context = modules["harbor.models.agent.context"]
    sys.modules.update(modules)


def _load_agent_module():
    _install_harbor_stubs()
    spec = importlib.util.spec_from_file_location("openclaw_external_agent_test", AGENT_FILE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _write_session(profile_dir: Path, agent_id: str, rounds):
    sessions = profile_dir / "agents" / agent_id / "sessions"
    sessions.mkdir(parents=True)
    session_file = sessions / "sess-1.jsonl"
    lines = []
    for inp, out in rounds:
        lines.append(
            json.dumps(
                {
                    "type": "message",
                    "message": {
                        "role": "assistant",
                        "usage": {
                            "input": inp,
                            "output": out,
                            "totalTokens": inp + out,
                        },
                    },
                }
            )
        )
    session_file.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return profile_dir


def test_total_input_is_not_the_sum_of_cumulative_inputs(tmp_path):
    module = _load_agent_module()
    agent = module.OpenClawExternalAgent()

    # Round inputs are cumulative: 100 then 300 means 200 NEW input tokens.
    _write_session(tmp_path, "main", [(100, 5), (300, 7)])

    report = agent._collect_session_tokens(str(tmp_path), "main")
    assert report is not None
    assert report["num_rounds"] == 2
    assert report["total_output"] == 12
    assert report["total_input"] == 300
    assert report["total_tokens"] == 312
    assert report["rounds"][0]["cumulative"] == 105
    assert report["rounds"][1]["cumulative"] == 312


def test_missing_session_returns_none(tmp_path):
    module = _load_agent_module()
    agent = module.OpenClawExternalAgent()
    assert agent._collect_session_tokens(str(tmp_path), "main") is None
