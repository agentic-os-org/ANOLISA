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

"""Make openclaw_external_agent importable in tests.

The harbor package is an external benchmark dependency that is not
installed in the unit-test environment; the agent module only needs its
class attributes at import time, so lightweight stubs suffice.
"""

import sys
import types
import logging


def _install_harbor_stubs() -> None:
    if "harbor" in sys.modules:
        return

    modules = {
        name: types.ModuleType(name)
        for name in [
            "harbor",
            "harbor.agents",
            "harbor.agents.base",
            "harbor.environments",
            "harbor.environments.base",
            "harbor.models",
            "harbor.models.agent",
            "harbor.models.agent.context",
        ]
    }

    class BaseAgent:
        def __init__(self, *args, **kwargs):
            self.model_name = kwargs.get("model_name", "openai/gpt-4o")
            self.logger = kwargs.get("logger") or logging.getLogger("stub")

    class BaseEnvironment:
        pass

    class AgentContext:
        pass

    modules["harbor.agents.base"].BaseAgent = BaseAgent
    modules["harbor.environments.base"].BaseEnvironment = BaseEnvironment
    modules["harbor.models.agent.context"].AgentContext = AgentContext
    sys.modules.update(modules)


_install_harbor_stubs()
