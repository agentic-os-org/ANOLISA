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

"""_try_parse_json must survive multi-object and trailing-brace output.

OpenClaw's ``--json`` mode prints one JSON object per event line. The
substring fallback spanned from the FIRST ``{`` to the LAST ``}`` in the
whole output, so two objects (or trailing text containing braces) were
stitched into one invalid JSON document and the parse returned None —
every command in the output was then lost and the loop re-prompted.
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


class OpenClawTestGlobals:
    """Holds the module under test without unittest attribute binding."""

    module = None

    @classmethod
    def parse(cls, text):
        return cls.module.OpenClawExternalAgent._try_parse_json(text)


class TryParseJsonTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        OpenClawTestGlobals.module = load_agent_module()

    def test_two_json_objects_returns_first(self):
        # OpenClaw --json emits one object per event; the old
        # first-'{'-to-last-'}' span produced
        # '{"payloads": [...]}\n{"meta": {...}}' — invalid JSON, None.
        text = (
            '{"payloads": [{"content": [{"type": "toolCall", '
            '"name": "exec", "arguments": {"command": "ls"}}]}]}\n'
            '{"meta": {"sessionId": "s-1"}}\n'
        )
        parsed = OpenClawTestGlobals.parse(text)
        self.assertIsNotNone(parsed, "two concatenated objects must not lose both")
        self.assertIn("payloads", parsed)

    def test_trailing_brace_noise_after_object(self):
        text = '{"meta": {"sessionId": "s-1"}}\n[debug] flushed queue (1) {dropped}\n'
        parsed = OpenClawTestGlobals.parse(text)
        self.assertIsNotNone(parsed)
        self.assertEqual(parsed["meta"]["sessionId"], "s-1")

    def test_leading_noise_single_object_still_parses(self):
        text = 'warn: connection reset, retrying\n{"payloads": [], "meta": {}}'
        parsed = OpenClawTestGlobals.parse(text)
        self.assertIsNotNone(parsed)

    def test_plain_garbage_still_none(self):
        self.assertIsNone(OpenClawTestGlobals.parse("no json here at all"))
        self.assertIsNone(OpenClawTestGlobals.parse(""))
        self.assertIsNone(OpenClawTestGlobals.parse('{"unterminated": true'))


if __name__ == "__main__":
    unittest.main()
