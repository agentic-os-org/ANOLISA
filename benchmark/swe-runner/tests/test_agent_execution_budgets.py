"""Reject invalid execution budgets before creating logs or runner resources."""

from __future__ import annotations

from pathlib import Path

import pytest
from pydantic import ValidationError
from typer.testing import CliRunner

from swe_runner import cli_commands
from swe_runner.cli import app
from swe_runner.common.models import AgentConfig, Settings


@pytest.mark.parametrize(
    "field, value", [("timeout", 0), ("timeout", -1), ("timeout", -60), ("step_limit", -1), ("step_limit", -100)]
)
def test_model_rejects_invalid_execution_budgets(field: str, value: int) -> None:
    with pytest.raises(ValidationError) as raised:
        AgentConfig(name="cosh", **{field: value})
    assert any(error["loc"] == (field,) for error in raised.value.errors())


@pytest.mark.parametrize(
    "option, value", [("timeout", 0), ("timeout", -1), ("timeout", -60), ("step-limit", -1), ("step-limit", -100)]
)
def test_cli_rejects_budgets_before_logging_or_session_allocation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, option: str, value: int
) -> None:
    effects: list[str] = []

    def logging(*args: object, **kwargs: object) -> None:
        effects.append("logging")

    def session(*args: object, **kwargs: object) -> None:
        effects.append("runner")
        raise RuntimeError("private allocation guard")

    monkeypatch.setattr(cli_commands, "setup_logging", logging)
    monkeypatch.setattr(cli_commands, "RunSession", session)
    destination = tmp_path / "output"
    result = CliRunner().invoke(app, ["run", "--agent", "cosh", f"--{option}={value}", "--output", str(destination)])
    assert result.exit_code != 0
    assert effects == []
    assert not destination.exists()


@pytest.mark.parametrize("name", ["cosh", "openclaw", "terminal"])
@pytest.mark.parametrize("timeout, steps", [(1, 0), (30, 1), (1800, 200)])
def test_valid_budgets_and_explicit_unlimited_steps_remain(name: str, timeout: int, steps: int) -> None:
    config = AgentConfig(name=name, timeout=timeout, step_limit=steps)
    assert (config.timeout, config.step_limit) == (timeout, steps)


def test_defaults_and_existing_numeric_coercion_remain() -> None:
    assert Settings().agent.timeout == 1800
    assert Settings().agent.step_limit == 0
    config = AgentConfig(name="cosh", timeout="15", step_limit="2")
    assert (config.timeout, config.step_limit) == (15, 2)
