"""CLI contract tests for ``skill-ledger analyze``."""

import json
from pathlib import Path

from agent_sec_cli.skill_ledger.cli import app
from typer.testing import CliRunner


def _write_skill(root: Path) -> None:
    (root / "SKILL.md").write_text(
        "---\nname: cli-test\ndescription: CLI test Skill\n---\nSafe content.\n",
        encoding="utf-8",
    )


def test_analyze_help_describes_read_only_behavior() -> None:
    result = CliRunner().invoke(app, ["analyze", "--help"])

    assert result.exit_code == 0
    assert (
        "Analyze current Skill content without creating or updating Skill Ledger state."
        in result.output
    )


def test_analyze_outputs_one_json_object_without_ledger_state(tmp_path: Path) -> None:
    _write_skill(tmp_path)

    result = CliRunner().invoke(app, ["analyze", str(tmp_path), "--format", "json"])

    assert result.exit_code == 0
    payload = json.loads(result.stdout)
    assert payload["schema_version"] == "1"
    assert payload["status"] == "pass"
    assert payload["coverage_complete"] is True
    assert not (tmp_path / ".skill-meta").exists()


def test_analyze_invalid_format_is_json_protocol_error(tmp_path: Path) -> None:
    _write_skill(tmp_path)

    result = CliRunner().invoke(app, ["analyze", str(tmp_path), "--format", "text"])

    assert result.exit_code == 2
    payload = json.loads(result.stdout)
    assert payload["status"] == "error"
    assert payload["errors"][0]["code"] == "unsupported-format"


def test_analyze_missing_root_argument_is_json_protocol_error() -> None:
    result = CliRunner().invoke(app, ["analyze"])

    assert result.exit_code == 2
    payload = json.loads(result.stdout)
    assert payload["errors"][0]["code"] == "skill-root-required"
