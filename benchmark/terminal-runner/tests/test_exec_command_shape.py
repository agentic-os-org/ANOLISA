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

"""_extract_toolcall_commands must only yield strings.

A malformed tool call whose ``arguments.command`` is a number or list
(the model is not required to emit valid JSON) used to append the raw
value; the agent loop then sliced it (\`cmd_str[:200]\`) and died with a
TypeError, taking the whole run down. The extractor must skip non-string
commands instead.
"""

import importlib.util
import sys
import types
import unittest
from pathlib import Path

AGENT_FILE = (
    Path(__file__).resolve().parents[1]
    / "external_agent"
    / "openclaw_external_agent.py"
)


def _install_harbor_stubs():
    if "harbor.agents.base" in sys.modules:
        return

    harbor_pkg = types.ModuleType("harbor")
    agents_pkg = types.ModuleType("harbor.agents")
    envs_pkg = types.ModuleType("harbor.environments")
    models_pkg = types.ModuleType("harbor.models")
    agent_models_pkg = types.ModuleType("harbor.models.agent")

    base_mod = types.ModuleType("harbor.agents.base")
    context_mod = types.ModuleType("harbor.models.agent.context")
    env_base_mod = types.ModuleType("harbor.environments.base")

    class BaseAgent:
        pass

    class AgentContext:
        pass

    class BaseEnvironment:
        pass

    base_mod.BaseAgent = BaseAgent
    context_mod.AgentContext = AgentContext
    env_base_mod.BaseEnvironment = BaseEnvironment

    harbor_pkg.agents = agents_pkg
    harbor_pkg.environments = envs_pkg
    harbor_pkg.models = models_pkg
    agents_pkg.base = base_mod
    envs_pkg.base = env_base_mod
    models_pkg.agent = agent_models_pkg
    agent_models_pkg.context = context_mod

    for name, module in (
        ("harbor", harbor_pkg),
        ("harbor.agents", agents_pkg),
        ("harbor.environments", envs_pkg),
        ("harbor.models", models_pkg),
        ("harbor.models.agent", agent_models_pkg),
        ("harbor.agents.base", base_mod),
        ("harbor.environments.base", env_base_mod),
        ("harbor.models.agent.context", context_mod),
    ):
        sys.modules[name] = module


def load_agent_module():
    _install_harbor_stubs()
    spec = importlib.util.spec_from_file_location(
        "openclaw_external_agent_under_test", AGENT_FILE
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ExecCommandShapeTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.module = load_agent_module()

    def extract(self, parsed):
        return self.module.OpenClawExternalAgent._extract_toolcall_commands(parsed)

    def test_non_string_payload_command_is_skipped(self):
        parsed = {
            "payloads": [
                {
                    "content": [
                        {
                            "type": "toolCall",
                            "name": "exec",
                            "arguments": {"command": 123},
                        },
                        {
                            "type": "toolCall",
                            "name": "exec",
                            "arguments": {"command": ["ls", "-la"]},
                        },
                        {
                            "type": "toolCall",
                            "name": "exec",
                            "arguments": {"command": "echo ok"},
                        },
                    ]
                }
            ]
        }
        commands = self.extract(parsed)
        self.assertEqual(commands, ["echo ok"])

    def test_non_string_meta_command_is_skipped(self):
        parsed = {
            "meta": {
                "toolCalls": [
                    {"name": "exec", "arguments": {"command": None}},
                    {"name": "exec", "arguments": {"command": "echo fine"}},
                ]
            }
        }
        self.assertEqual(self.extract(parsed), ["echo fine"])

    def test_result_survives_loop_slicing(self):
        """Everything the extractor yields must be sliceable like a string."""
        parsed = {
            "payloads": [
                {"content": [
                    {"type": "toolCall", "name": "exec", "arguments": {"command": 7}},
                ]}
            ]
        }
        for cmd in self.extract(parsed):
            self.assertIs(type(cmd), str)
            _ = cmd[:200]  # must not raise


if __name__ == "__main__":
    unittest.main()
