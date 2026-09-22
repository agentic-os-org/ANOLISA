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

"""Tests for prompt builder module."""

from pathlib import Path
from unittest.mock import patch

import pytest

from swe_runner.agents.openclaw.prompts import build_openclaw_agents_text, build_openclaw_prompt
from swe_runner.common.models import SWEInstance
from swe_runner.run.prompting.prompt_resources import BUILTIN_SKILL_PATH, load_builtin_skill_text
from swe_runner.run.prompting.prompts import build_prompt


def _make_instance() -> SWEInstance:
    return SWEInstance(
        instance_id="django__django-12345",
        repo="django/django",
        version="3.2",
        base_commit="abc123",
        problem_statement="Fix the bug in the ORM layer",
        patch="diff --git a/file.py",
        test_patch="diff --git a/test_file.py",
    )


class TestBuildPrompt:
    """Tests for build_prompt function."""

    def test_prompt_contains_problem_statement(self) -> None:
        """Verify the problem text appears in output."""
        prompt = build_prompt(
            instance=_make_instance(),
            work_dir=Path("/testbed"),
            container_name="test-container",
        )

        assert "Fix the bug in the ORM layer" in prompt

    def test_prompt_omits_skill_by_default(self) -> None:
        """Verify the skill instruction is omitted by default."""
        prompt = build_prompt(
            instance=_make_instance(),
            work_dir=Path("/testbed"),
            container_name="test-container",
        )

        assert "Use the `swe-bench-patch-generation` skill for this task." not in prompt
        assert prompt.startswith("<pr_description>")

    def test_prompt_can_include_skill_instruction(self, tmp_path: Path) -> None:
        """Verify callers can include the opening skill instruction."""
        skills_dir = tmp_path / "skills"
        skill_path = skills_dir / "swe-bench-patch-generation" / "SKILL.md"
        skill_path.parent.mkdir(parents=True)
        skill_path.write_text("# Skill\n", encoding="utf-8")
        prompt = build_prompt(
            instance=_make_instance(),
            work_dir=Path("/testbed"),
            container_name="test-container",
            use_skill=True,
            skills_dir=skills_dir,
        )

        assert prompt.startswith("Use the `swe-bench-patch-generation` skill for this task.")

    def test_prompt_skips_skill_instruction_when_skill_is_not_bundled(self) -> None:
        """Open-source builds should not inject unusable skill guidance."""
        prompt = build_prompt(
            instance=_make_instance(),
            work_dir=Path("/testbed"),
            container_name="test-container",
            use_skill=True,
        )

        assert not prompt.startswith("Use the `swe-bench-patch-generation` skill for this task.")
        assert "Use the `swe-bench-patch-generation` skill for this task." not in prompt
        assert prompt.startswith("<pr_description>")

    def test_prompt_can_omit_skill_instruction(self) -> None:
        """Verify callers can remove the opening skill instruction."""
        prompt = build_prompt(
            instance=_make_instance(),
            work_dir=Path("/testbed"),
            container_name="test-container",
            use_skill=False,
        )

        assert not prompt.startswith("Use the `swe-bench-patch-generation` skill for this task.")
        assert "Use the `swe-bench-patch-generation` skill for this task." not in prompt
        assert prompt.startswith("<pr_description>")

    def test_prompt_contains_work_dir(self) -> None:
        """Verify work_dir path is included."""
        prompt = build_prompt(
            instance=_make_instance(),
            work_dir=Path("/testbed"),
            container_name="test-container",
        )

        assert "/testbed" in prompt

    def test_prompt_contains_container_name(self) -> None:
        """Verify container_name appears in the prompt."""
        prompt = build_prompt(
            instance=_make_instance(),
            work_dir=Path("/testbed"),
            container_name="test-container",
        )

        assert "test-container" in prompt

    def test_prompt_does_not_contain_container_id(self) -> None:
        """Verify container_id (hash) does NOT appear in the prompt."""
        prompt = build_prompt(
            instance=_make_instance(),
            work_dir=Path("/testbed"),
            container_name="test-container",
        )

        # The old "(ID: <container_id>)" line should be gone
        assert "(ID:" not in prompt
        assert "Docker container: `test-container`" in prompt

    def test_prompt_contains_repo_info(self) -> None:
        """Verify repo name and base commit are included."""
        prompt = build_prompt(
            instance=_make_instance(),
            work_dir=Path("/testbed"),
            container_name="test-container",
        )

        assert "django/django" in prompt
        assert "abc123" in prompt

    def test_prompt_contains_test_command(self) -> None:
        """Verify prompt contains generic docker exec command using container_name."""
        prompt = build_prompt(
            instance=_make_instance(),
            work_dir=Path("/testbed"),
            container_name="test-container",
        )

        assert "docker exec -w /testbed test-container bash -c '<command>'" in prompt

    def test_prompt_requires_host_file_tools_and_container_shell(self) -> None:
        """Verify host-vs-container tool boundary is explicit."""
        prompt = build_prompt(
            instance=_make_instance(),
            work_dir=Path("/testbed"),
            container_name="test-container",
        )

        assert "Repository path on host: `/testbed`" in prompt
        assert "Docker container: `test-container`" in prompt
        assert "The repository is mounted into the container at the exact same absolute path: `/testbed`" in prompt
        assert "Use host-side non-shell file tools only." in prompt
        assert "Host-side file edits under `/testbed` are immediately visible inside the container." in prompt
        assert "This is the ONLY valid shell/runtime environment for this task." in prompt
        assert "The container provides the correct runtime, dependencies, interpreter, and test environment." in prompt
        assert "The container already has a Conda environment named `testbed` for running Python and tests." in prompt
        assert "ALWAYS run shell commands with:" in prompt
        assert "`docker exec -w /testbed test-container bash -c '<command>'`" in prompt
        assert (
            "`docker exec -w /testbed test-container bash -c 'source /opt/miniconda3/etc/profile.d/conda.sh && conda activate testbed && <command>'`"
            in prompt
        )
        assert "Do NOT use host shell even though the path is the same." in prompt

    def test_prompt_contains_shell_decision_rules(self) -> None:
        """Verify shell-vs-file-tool rules are explicit and conservative."""
        prompt = build_prompt(
            instance=_make_instance(),
            work_dir=Path("/testbed"),
            container_name="test-container",
        )

        assert "Treat anything of the following form as a shell command and run it only in the container:" in prompt
        assert "- `pytest`" in prompt
        assert "- `conda`" in prompt
        assert "- `git`" in prompt
        assert "- any command entered into a terminal/shell" in prompt
        assert "If it is a host-side file read/edit/search tool, it may operate directly on `/testbed`." in prompt
        assert "If it is a shell command, it MUST go through `docker exec`." in prompt
        assert "When unsure, treat it as a shell command and run it in the container." in prompt
        assert (
            "Do NOT probe the Python environment with commands like `which python`, `python --version`, `pip list`, or `conda env list` unless strictly necessary."
            in prompt
        )

    def test_prompt_contains_valid_and_invalid_examples(self) -> None:
        """Verify examples reinforce allowed and forbidden actions."""
        prompt = build_prompt(
            instance=_make_instance(),
            work_dir=Path("/testbed"),
            container_name="test-container",
        )

        assert "Valid:" in prompt
        assert "Read/edit `/testbed/pkg/module.py` with host-side file tools" in prompt
        assert (
            "`docker exec -w /testbed test-container bash -c 'source /opt/miniconda3/etc/profile.d/conda.sh && conda activate testbed && python -m pytest path/to/test.py -x -v'`"
            in prompt
        )
        assert "`docker exec -w /testbed test-container bash -c 'pytest path/to/test.py -x -v'`" in prompt
        assert "Invalid:" in prompt
        assert "Running `pytest`, `python`, or `git` directly on the host" in prompt
        assert "Rewriting container paths into different host paths" in prompt

    def test_prompt_submission_commands_use_docker_exec(self) -> None:
        """Verify submission guidance avoids host-side shell instructions."""
        prompt = build_prompt(
            instance=_make_instance(),
            work_dir=Path("/testbed"),
            container_name="test-container",
        )

        assert "After fixing and verifying, generate a patch." in prompt
        assert "Do NOT include temporary test files or helper scripts in the final patch." in prompt
        assert "Do NOT call any submit tool." in prompt
        assert "git add <files>" not in prompt
        assert "git format-patch -1 HEAD --stdout > patch.txt" not in prompt

    def test_prompt_warns_against_submit_tools(self) -> None:
        """Verify prompt warns against submit tools and temp-file patch noise."""
        prompt = build_prompt(
            instance=_make_instance(),
            work_dir=Path("/testbed"),
            container_name="test-container",
        )

        assert "Do NOT include temporary test files or helper scripts in the final patch." in prompt
        assert "Do NOT call any submit tool." in prompt

    def test_prompt_with_special_characters(self) -> None:
        """Problem statement with quotes/newlines doesn't break."""
        instance = SWEInstance(
            instance_id="django__django-12345",
            repo="django/django",
            version="3.2",
            base_commit="abc123",
            problem_statement="Fix the \"bug\" with\nnewlines and 'quotes'",
            patch="diff --git a/file.py",
            test_patch="diff --git a/test_file.py",
        )
        prompt = build_prompt(
            instance=instance,
            work_dir=Path("/testbed"),
            container_name="test-container",
        )

        assert "Fix the \"bug\" with\nnewlines and 'quotes'" in prompt
        assert "<pr_description>" in prompt
        assert "</pr_description>" in prompt
        assert "<instructions>" in prompt
        assert "</instructions>" in prompt

    def test_prompt_is_stable_across_identical_calls(self) -> None:
        """Same inputs produce the same prompt (deterministic)."""
        instance = _make_instance()
        p1 = build_prompt(instance, Path("/testbed"), "my-container")
        p2 = build_prompt(instance, Path("/testbed"), "my-container")
        assert p1 == p2

    def test_openclaw_prompt_starts_with_task_request(self) -> None:
        """OpenClaw user prompt should start with the concrete task request."""
        prompt = build_openclaw_prompt(
            instance=_make_instance(),
        )

        assert prompt.startswith("Solve the following issue.\n\n<pr_description>")
        assert "Fix the issue described above." not in prompt
        assert "<instructions>" not in prompt
        assert "</instructions>" not in prompt
        assert "/workspace/patch.txt" not in prompt
        assert "git format-patch -1 HEAD --stdout > /workspace/patch.txt" not in prompt

    def test_openclaw_agents_text_contains_shared_guidance(self) -> None:
        """OpenClaw shared constraints should live in AGENTS.md."""
        agents_text = build_openclaw_agents_text()

        assert agents_text.startswith("# Repository Task Guidelines")
        assert "The repository working tree is `/testbed`." in agents_text
        assert "Work only inside `/testbed`." in agents_text
        assert (
            "Use the available tools to inspect source files, make the minimal source change, and verify the fix."
            in agents_text
        )
        assert (
            "The environment is already prepared. Do not reconfigure it, install packages, or probe it unless necessary."
            in agents_text
        )
        assert "You may read tests and run relevant tests. Treat tests as verification assets." in agents_text
        assert (
            "If you create temporary reproduction or verification files, delete them before finishing." in agents_text
        )
        assert "Prefer focused verification over broad test suites." in agents_text
        assert (
            "Do not leave changes to tests, documentation, build/config files, generated files, caches, or temporary helper scripts in the final working tree."
            in agents_text
        )
        assert "Do not run `git add`." in agents_text
        assert "Do not run `git commit`." in agents_text
        assert "Do not create patch files, diff files, or submission artifacts." in agents_text
        assert "Before finishing, run `cd /testbed && git diff` to inspect your changes." in agents_text
        assert "Confirm the diff contains only the intended source changes." in agents_text

    def test_openclaw_agents_text_can_include_skill(self) -> None:
        """OpenClaw skill text should be appended after shared AGENTS.md guidance."""
        agents_text = build_openclaw_agents_text(skill_text="# Skill\n\nDo X")

        assert agents_text.startswith("# Repository Task Guidelines")
        assert agents_text.endswith("# Skill\n\nDo X")

    def test_builtin_skill_reports_missing_resource(self) -> None:
        """Open-source builds should report clearly when no skill resource is configured."""
        assert not BUILTIN_SKILL_PATH.is_file()
        assert BUILTIN_SKILL_PATH.parent.name == "swe-bench-patch-generation"
        assert BUILTIN_SKILL_PATH.parent.parent.name == "skills"

        with pytest.raises(FileNotFoundError, match="SWE-bench skill file is not available"):
            load_builtin_skill_text()

    def test_openclaw_prompt_does_not_expose_runner_or_benchmark_details(self) -> None:
        """OpenClaw prompt should keep implementation details out of the agent task."""
        prompt = build_openclaw_prompt(
            instance=_make_instance(),
        )

        assert "SWE-bench" not in prompt
        assert "runner" not in prompt.lower()
        assert "model_patch" not in prompt
        assert "test_patch" not in prompt
        assert "BOOTSTRAP" not in prompt
        assert "per-case" not in prompt

    def test_openclaw_prompt_appends_custom_prompt(self, tmp_path: Path) -> None:
        """OpenClaw receives per-case prompt guidance at the end of the user prompt."""
        prompts_dir = tmp_path / "prompts"
        prompts_dir.mkdir()
        (prompts_dir / "django__django-12345").write_text(
            "CUSTOM CASE GUIDANCE\nRead /testbed/pkg/module.py",
            encoding="utf-8",
        )
        prompt = build_openclaw_prompt(
            instance=_make_instance(),
            use_per_case_prompt=True,
            prompts_dir=prompts_dir,
        )
        assert "<custom_instructions>" not in prompt
        assert "<task_guidance>" in prompt
        assert "CUSTOM CASE GUIDANCE" in prompt
        assert "Read /testbed/pkg/module.py" in prompt
        assert prompt.index("</pr_description>") < prompt.index("<task_guidance>")
        assert prompt.endswith("CUSTOM CASE GUIDANCE\nRead /testbed/pkg/module.py\n\n</task_guidance>")

    def test_openclaw_prompt_skips_missing_custom_prompt(self) -> None:
        """Open-source builds should keep running when per-case prompt files are absent."""
        with patch("swe_runner.agents.openclaw.prompts.load_custom_prompt", return_value=None):
            prompt = build_openclaw_prompt(
                instance=_make_instance(),
                use_per_case_prompt=True,
            )

        assert "<task_guidance>" not in prompt
        assert prompt.startswith("Solve the following issue.\n\n<pr_description>")

    def test_openclaw_prompt_does_not_include_skill_instruction(self) -> None:
        """OpenClaw receives skill guidance through AGENTS.md, not user prompt."""
        prompt = build_openclaw_prompt(
            instance=_make_instance(),
        )

        assert not prompt.startswith("Use the /workspace/skills/swe-bench-patch-generation/SKILL.md skill")
        assert "/workspace/skills/swe-bench-patch-generation/SKILL.md skill for this task." not in prompt
        assert prompt.startswith("Solve the following issue.\n\n<pr_description>")

    def test_openclaw_prompt_does_not_expose_host_or_docker_exec_in_examples(self) -> None:
        """OpenClaw prompt examples should reflect plugin-managed execution."""
        prompt = build_openclaw_prompt(
            instance=_make_instance(),
        )

        assert "Repository path on host: `/testbed`" not in prompt
        assert "Docker container: `test-container`" not in prompt
        assert "docker exec -w /testbed test-container bash -c '<command>'" not in prompt
        assert (
            "Use the available tools to inspect source files, make the minimal source change, and verify the fix."
            not in prompt
        )
