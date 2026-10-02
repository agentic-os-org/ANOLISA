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

"""OpenClaw prompt and AGENTS.md guidance builders."""

from __future__ import annotations

import logging
from pathlib import Path

from swe_runner.common.models import SWEInstance
from swe_runner.run.prompting.prompt_resources import load_custom_prompt

logger = logging.getLogger(__name__)

OPENCLAW_COMMON_AGENTS_TEXT = """# Repository Task Guidelines

You are working on a single repository issue. The repository working tree is `/testbed`.

## Workspace

- Work only inside `/testbed`.
- Use the available tools to inspect source files, make the minimal source change, and verify the fix.
- The environment is already prepared. Do not reconfigure it, install packages, or probe it unless necessary.

## Verification

- You may read tests and run relevant tests. Treat tests as verification assets.
- Prefer focused verification over broad test suites.
- If you create temporary reproduction or verification files, delete them before finishing.

## Final Working Tree

- Do not leave changes to tests, documentation, build/config files, generated files, caches, or temporary helper scripts in the final working tree.
- Do not run `git add`.
- Do not run `git commit`.
- Do not create patch files, diff files, or submission artifacts.
- Before finishing, run `cd /testbed && git diff` to inspect your changes.
- Confirm the diff contains only the intended source changes.
"""


def build_openclaw_agents_text(*, skill_text: str | None = None) -> str:
    """Build the AGENTS.md content injected into each OpenClaw workspace."""
    parts = [OPENCLAW_COMMON_AGENTS_TEXT.strip()]
    if skill_text:
        parts.append(skill_text.strip())
    return "\n\n".join(parts)


def build_openclaw_prompt(
    instance: SWEInstance,
    *,
    use_per_case_prompt: bool = False,
    prompts_dir: Path | None = None,
) -> str:
    """Build the OpenClaw user prompt for one SWE-bench instance."""
    prompt = f"""Solve the following issue.

<pr_description>

Repository: {instance.repo}
Issue ID: {instance.instance_id}
Base Commit: {instance.base_commit}

{instance.problem_statement}
</pr_description>"""
    if use_per_case_prompt:
        custom_prompt = load_custom_prompt(instance.instance_id, prompts_dir=prompts_dir)
        if custom_prompt:
            prompt = f"{prompt}\n\n<task_guidance>\n\n{custom_prompt}\n\n</task_guidance>"
        else:
            logger.warning("PER_CASE_PROMPT_SKIPPED instance=%s reason=prompt_missing_or_empty", instance.instance_id)
    return prompt
