"""Unreadable optional guidance must not abort prompt construction."""

from pathlib import Path

import pytest

from swe_runner.agents.openclaw.prompts import build_openclaw_prompt
from swe_runner.common.models import SWEInstance
from swe_runner.run.prompting import prompt_resources
from swe_runner.run.prompting.prompts import build_prompt


@pytest.mark.parametrize("payload", [b"\xff", b"valid prefix\xe4\xb8"])
def test_optional_custom_prompt_falls_back(tmp_path: Path, caplog: pytest.LogCaptureFixture, payload: bytes) -> None:
    resource = tmp_path / "fixture__case-1"
    resource.write_bytes(payload)
    assert prompt_resources.load_custom_prompt(resource.name, prompts_dir=tmp_path) is None
    assert "CUSTOM_PROMPT_LOAD_FAILED" in caplog.text
    assert str(resource) in caplog.text


@pytest.mark.parametrize("payload", [b"\xff", b"valid prefix\xe4\xb8"])
def test_required_custom_prompt_attributes_decode_failure(tmp_path: Path, payload: bytes) -> None:
    resource = tmp_path / "fixture__case-1"
    resource.write_bytes(payload)
    with pytest.raises(RuntimeError, match="Failed to load per-case prompt") as caught:
        prompt_resources.load_required_custom_prompt(resource.name, prompts_dir=tmp_path)
    assert str(resource) in str(caught.value)
    assert isinstance(caught.value.__cause__, UnicodeDecodeError)


@pytest.mark.parametrize("payload", [b"\xff", b"valid prefix\xe4\xb8"])
def test_builtin_skill_decode_failure_keeps_required_and_optional_contracts(
    tmp_path: Path, caplog: pytest.LogCaptureFixture, payload: bytes
) -> None:
    resource = prompt_resources.resolve_builtin_skill_path(tmp_path)
    resource.parent.mkdir()
    resource.write_bytes(payload)
    with pytest.raises(RuntimeError, match="Failed to load SWE-bench skill") as caught:
        prompt_resources.load_builtin_skill_text(skills_dir=tmp_path)
    assert str(resource) in str(caught.value)
    assert isinstance(caught.value.__cause__, UnicodeDecodeError)
    assert prompt_resources.load_optional_builtin_skill_text(skills_dir=tmp_path) is None
    assert "BUILTIN_SKILL_UNAVAILABLE" in caplog.text


def test_prompt_builders_continue_without_undecodable_override(tmp_path: Path, sample_instance: SWEInstance) -> None:
    (tmp_path / sample_instance.instance_id).write_bytes(b"\xff")
    cosh_prompt = build_prompt(
        instance=sample_instance,
        work_dir=Path("/testbed"),
        container_name="fixture",
        use_per_case_prompt=True,
        prompts_dir=tmp_path,
    )
    openclaw_prompt = build_openclaw_prompt(sample_instance, use_per_case_prompt=True, prompts_dir=tmp_path)
    for prompt in (cosh_prompt, openclaw_prompt):
        assert sample_instance.problem_statement in prompt
        assert "<custom_instructions>" not in prompt
        assert "<task_guidance>" not in prompt
        assert "\ufffd" not in prompt


def test_valid_multilingual_resources_are_unchanged(tmp_path: Path) -> None:
    text = "Review the Unicode example: \u4e2d\u6587 \U0001f431"
    resource = tmp_path / "fixture__case-1"
    resource.write_text(f"\n{text}\n", encoding="utf-8")
    skill = prompt_resources.resolve_builtin_skill_path(tmp_path)
    skill.parent.mkdir()
    skill.write_text(f"\n{text}\n", encoding="utf-8")
    assert prompt_resources.load_custom_prompt(resource.name, prompts_dir=tmp_path) == text
    assert prompt_resources.load_required_custom_prompt(resource.name, prompts_dir=tmp_path) == text
    assert prompt_resources.load_builtin_skill_text(skills_dir=tmp_path) == text
    assert prompt_resources.load_optional_builtin_skill_text(skills_dir=tmp_path) == text
