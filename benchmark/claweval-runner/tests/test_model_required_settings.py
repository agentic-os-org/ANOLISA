"""Reject unusable model settings before changing any configuration file."""

import os
import subprocess
import sys
from pathlib import Path

import pytest
import yaml

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "configure_model.py"
CONFIG_FILES = (
    "config.yaml",
    "config_general.yaml",
    "config_multimodal.yaml",
    "config_user_agent.yaml",
)


def run_config(
    directory: Path, arguments: list[str], stdin: str = "", judge_default: str | None = None
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env.pop("JUDGE_MODEL_ID", None)
    if judge_default is not None:
        env["JUDGE_MODEL_ID"] = judge_default
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--config-dir", str(directory), *arguments],
        input=stdin,
        capture_output=True,
        text=True,
        env=env,
        timeout=10,
    )


@pytest.mark.parametrize(
    "arguments,field",
    [
        (["--api-key", "   "], "api_key"),
        (["--api-key", "fixture-key", "--base-url", " \t "], "base_url"),
        (["--api-key", "fixture-key", "--model-id", "   "], "model_id"),
        (["--api-key", "fixture-key", "--judge-model-id", "   "], "model_id"),
    ],
)
def test_invalid_cli_preserves_all_files(tmp_path: Path, arguments: list[str], field: str) -> None:
    original = b"# retained configuration\njudge:\n  enabled: false\n"
    for name in CONFIG_FILES:
        (tmp_path / name).write_bytes(original)
    result = run_config(tmp_path, arguments)
    assert result.returncode == 1
    assert field in result.stderr
    assert "fixture-key" not in result.stderr
    assert all((tmp_path / name).read_bytes() == original for name in CONFIG_FILES)


@pytest.mark.parametrize("judge_key,user_key", [("", "user-key"), ("judge-key", "")])
def test_invalid_interactive_role_preserves_all_files(
    tmp_path: Path, judge_key: str, user_key: str
) -> None:
    original = b"model: {model_id: previous}\n"
    for name in CONFIG_FILES:
        (tmp_path / name).write_bytes(original)
    answers = ["model-key", "n", judge_key, user_key, *([""] * 6)]
    result = run_config(tmp_path, [], "\n".join(answers) + "\n")
    assert result.returncode == 1
    assert "api_key" in result.stderr
    assert all((tmp_path / name).read_bytes() == original for name in CONFIG_FILES)


def test_empty_interactive_judge_default_does_not_create_config(tmp_path: Path) -> None:
    result = run_config(tmp_path, [], "model-key\ny\n" + "\n" * 6, judge_default="")
    assert result.returncode == 1
    assert "judge.model_id" in result.stderr
    assert list(tmp_path.iterdir()) == []


def test_invalid_cli_does_not_create_default_config(tmp_path: Path) -> None:
    result = run_config(tmp_path, ["--api-key", "   "])
    assert result.returncode == 1
    assert list(tmp_path.iterdir()) == []


def test_valid_cli_keeps_defaults_and_existing_options(tmp_path: Path) -> None:
    for name in CONFIG_FILES:
        (tmp_path / name).write_text("judge: {enabled: false}\nretained: yes\n")
    result = run_config(tmp_path, ["--api-key", "fixture-key"])
    assert result.returncode == 0, result.stderr
    for name in CONFIG_FILES:
        config = yaml.safe_load((tmp_path / name).read_text())
        assert config["judge"]["enabled"] is False
        assert config["model"]["input_modalities"] == ["text", "image"]
        assert config["model"]["api_key"] == "fixture-key"
        assert config["retained"] is True


def test_valid_interactive_keeps_separate_role_keys(tmp_path: Path) -> None:
    answers = ["model-key", "n", "judge-key", "user-key", *([""] * 6)]
    result = run_config(tmp_path, [], "\n".join(answers) + "\n")
    assert result.returncode == 0, result.stderr
    config = yaml.safe_load((tmp_path / "config.yaml").read_text())
    assert config["model"]["api_key"] == "model-key"
    assert config["judge"]["api_key"] == "judge-key"
    assert config["user_agent_model"]["api_key"] == "user-key"
