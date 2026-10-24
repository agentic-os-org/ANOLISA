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

"""Trailing-comment stripping must be quote-aware.

The previous ``line[:line.index(" #")]`` cut at the first " #" even inside
quotes, so ``echo "revenue #1 in Q3" > report.txt`` became the truncated
command ``echo "revenue`` in the auto-generated skill hint. All cases go
through the public entrypoint (_auto_skill_from_solve).
"""

import sys
import types
import logging
from pathlib import Path

# harbor is an external benchmark dependency not installed in the unit-test
# environment; the agent module only needs class attributes at import time.
if "harbor" not in sys.modules:
    _mods = {
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

    class _BaseAgent:
        def __init__(self, *args, **kwargs):
            self.model_name = kwargs.get("model_name", "openai/gpt-4o")
            self.logger = kwargs.get("logger") or logging.getLogger("stub")

    _mods["harbor.agents.base"].BaseAgent = _BaseAgent
    _mods["harbor.environments.base"].BaseEnvironment = type("_BE", (), {})
    _mods["harbor.models.agent.context"].AgentContext = type("_AC", (), {})
    sys.modules.update(_mods)

sys.path.insert(0, str(Path(__file__).resolve().parent))

from openclaw_external_agent import OpenClawExternalAgent  # noqa: E402


def _skill(solve: str, tmp_path: Path) -> str:
    solve_path = tmp_path / "solve.sh"
    solve_path.write_text(solve, encoding="utf-8")
    return OpenClawExternalAgent._auto_skill_from_solve(str(solve_path))


class TestQuotedHashIsContent:
    def test_double_quoted_hash_command_kept_whole(self, tmp_path):
        skill = _skill(
            "#!/bin/bash\n"
            'echo "revenue #1 in Q3" > report.txt\n'
            "echo next\n",
            tmp_path,
        )
        assert "revenue #1 in Q3" in skill
        assert '`echo "revenue`' not in skill

    def test_single_quoted_hash_command_kept_whole(self, tmp_path):
        skill = _skill(
            "#!/bin/bash\n"
            "sed 's/old #value/new/' config.ini\n",
            tmp_path,
        )
        assert "s/old #value/new/" in skill
        assert "`sed 's/old`" not in skill


class TestGenuineCommentsStillStripped:
    def test_trailing_comment_removed(self, tmp_path):
        skill = _skill(
            "#!/bin/bash\n"
            "yum install -y foo # install the package\n",
            tmp_path,
        )
        assert "yum install -y foo" in skill
        assert "# install the package" not in skill

    def test_no_comment_untouched(self, tmp_path):
        skill = _skill("#!/bin/bash\necho done\n", tmp_path)
        assert "echo done" in skill
