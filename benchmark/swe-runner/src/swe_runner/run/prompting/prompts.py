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

"""Agent prompt construction."""

from __future__ import annotations

import logging
from pathlib import Path

from swe_runner.common.models import SWEInstance
from swe_runner.run.prompting.prompt_resources import builtin_skill_available, load_custom_prompt

logger = logging.getLogger(__name__)


def build_prompt(
    instance: SWEInstance,
    work_dir: Path,
    container_name: str,
    *,
    agent_name: str = "cosh",
    use_skill: bool = False,
    skills_dir: Path | None = None,
    use_per_case_prompt: bool = False,
    prompts_dir: Path | None = None,
) -> str:
    """Build a prompt for Docker-backed agents with problem context and instructions."""
    del agent_name

    custom_prompt = load_custom_prompt(instance.instance_id, prompts_dir=prompts_dir) if use_per_case_prompt else None
    skill_instruction = ""
    if use_skill:
        if builtin_skill_available(skills_dir=skills_dir):
            skill_instruction = "Use the `swe-bench-patch-generation` skill for this task.\n\n"
        else:
            logger.warning("SKILL_GUIDANCE_SKIPPED reason=builtin_skill_missing")
    custom_section = f"<custom_instructions>\n\n{custom_prompt}\n\n</custom_instructions>\n\n" if custom_prompt else ""
    return f"""{custom_section}{skill_instruction}<pr_description>

Repository: {instance.repo}
Issue ID: {instance.instance_id}
Base Commit: {instance.base_commit}

{instance.problem_statement}
</pr_description>

<instructions>
Fix the issue described above.

## Environment

- Repository path on host: `{work_dir}`
- Docker container: `{container_name}`
- The repository is mounted into the container at the exact same absolute path: `{work_dir}`

### Execution model

There are two different execution contexts:

1. Host context
   - Use host-side non-shell file tools only.
   - Allowed on host: reading files, editing files, non-shell file search/navigation.
   - Host-side file edits under `{work_dir}` are immediately visible inside the container.

2. Container context
   - This is the ONLY valid shell/runtime environment for this task.
   - The container provides the correct runtime, dependencies, interpreter, and test environment.
   - The container already has a Conda environment named `testbed` for running Python and tests.
   - Therefore, every shell command must run inside the container.

## Hard rules
- NEVER run shell commands on the host against `{work_dir}`.
- ALWAYS run shell commands with:
  `docker exec -w {work_dir} {container_name} bash -c '<command>'`
- For Python or pytest commands, prefer:
  `docker exec -w {work_dir} {container_name} bash -c 'source /opt/miniconda3/etc/profile.d/conda.sh && conda activate testbed && <command>'`
- Paths are identical on host and in container.
- If a command prints a path like `{work_dir}/foo/bar.py`, use that exact same path with host-side file tools.
- Do NOT translate paths.
- Do NOT use host shell even though the path is the same.
- Do NOT probe the Python environment with commands like `which python`, `python --version`, `pip list`, or `conda env list` unless strictly necessary.

Treat anything of the following form as a shell command and run it only in the container:
- `pytest`
- `conda`
- `git`
- any command entered into a terminal/shell

If it is a host-side file read/edit/search tool, it may operate directly on `{work_dir}`.
If it is a shell command, it MUST go through `docker exec`.

## Decision rule

Before every action, apply this rule:
- If the action is file reading/editing/search using non-shell tools, use the host path `{work_dir}` directly.
- If the action is any shell/terminal command, run it in the container.
- When unsure, treat it as a shell command and run it in the container.

## Examples

Valid:
- Read/edit `{work_dir}/pkg/module.py` with host-side file tools
- `docker exec -w {work_dir} {container_name} bash -c 'source /opt/miniconda3/etc/profile.d/conda.sh && conda activate testbed && python -m pytest path/to/test.py -x -v'`
- `docker exec -w {work_dir} {container_name} bash -c 'pytest path/to/test.py -x -v'`
- `docker exec -w {work_dir} {container_name} bash -c 'git diff'`

Invalid:
- Running `pytest`, `python`, or `git` directly on the host
- Running `ls`, `find`, or `grep` directly on the host shell
- Rewriting container paths into different host paths
- Using host shell just because the path looks identical

## Submission

After fixing and verifying, generate a patch.

Do NOT include temporary test files or helper scripts in the final patch.
Do NOT call any submit tool.
</instructions>"""
