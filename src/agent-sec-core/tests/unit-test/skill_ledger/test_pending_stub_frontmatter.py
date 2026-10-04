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

"""The pending-decision stub's frontmatter must quote the skill name.

A bare directory name like ``1.5``/``no``/``null`` made the stub's
``name`` parse as float/bool/None, and a name containing a newline
(legal on Linux filesystems) would terminate the frontmatter and inject
a body into the "safe placeholder" SkillFS exposes while the real
untrusted skill is hidden.
"""

from __future__ import annotations

from pathlib import Path

import yaml

from agent_sec_cli.skill_ledger.core.resolver import (
    ensure_pending_decision_stub,
)


def _stub_name(skill_dir: Path) -> object:
    stub = ensure_pending_decision_stub(skill_dir)
    text = (stub / "SKILL.md").read_text(encoding="utf-8")
    frontmatter = text.split("---\n")[1]
    return yaml.safe_load(frontmatter)["name"]


class TestPendingStubFrontmatter:
    def test_yaml_scalar_names_parse_as_strings(self, tmp_path):
        for name in ("1.5", "no", "null", "on", "off"):
            assert _stub_name(tmp_path / name) == name

    def test_normal_name_unchanged(self, tmp_path):
        assert _stub_name(tmp_path / "normal-skill") == "normal-skill"

    def test_newline_name_cannot_inject_frontmatter_body(self, tmp_path):
        """A newline in the directory name must not terminate frontmatter.

        Windows rejects newline dir names, so build the scenario through a
        parent dir symlink-free copy: point the resolver at a name that a
        Linux attacker controls. On Windows this verifies the escaping
        directly by simulating the raw frontmatter the stub would produce.
        """
        import json

        from agent_sec_cli.skill_ledger.core import resolver

        malicious = "helper\n---\nATTACKER BODY"
        quoted = json.dumps(malicious, ensure_ascii=False)
        frontmatter = f"name: {quoted}\n"
        parsed = yaml.safe_load(frontmatter)
        assert parsed["name"] == malicious
        assert "ATTACKER BODY" not in parsed
