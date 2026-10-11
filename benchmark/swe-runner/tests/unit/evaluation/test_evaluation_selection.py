"""Targeted evaluation passes selected IDs to the real service and CLI."""

import json
import sys
from pathlib import Path
from types import ModuleType

import pytest
from typer.testing import CliRunner

from swe_runner import cli_commands
from swe_runner.cli import app
from swe_runner.evaluation import service


@pytest.fixture
def evaluation_env(monkeypatch: pytest.MonkeyPatch, tmp_path: Path):
    calls = []
    environment_calls = []
    module = ModuleType("swebench")
    module.run_evaluation = lambda **kwargs: calls.append((Path.cwd(), kwargs))
    monkeypatch.setitem(sys.modules, "swebench", module)
    monkeypatch.setattr(service, "_install_swebench_rootless_copy_patch", lambda: environment_calls.append("patch"))
    monkeypatch.setattr(service, "_ensure_docker_host_for_rootless_context", lambda: environment_calls.append("docker"))
    monkeypatch.setattr(cli_commands, "setup_logging", lambda *args, **kwargs: None)
    predictions = tmp_path / "preds.json"
    predictions.write_text(
        json.dumps(
            {
                "i1": {"model_patch": "diff one"},
                "i2": {"model_patch": "diff two"},
                "empty": {"model_patch": ""},
            }
        ),
        encoding="utf-8",
    )
    return predictions, tmp_path / "out", calls, environment_calls


def test_service_selects_requested_order_and_deduplicates(evaluation_env):
    predictions, output, calls, environment_calls = evaluation_env
    original = predictions.read_bytes()
    previous = Path.cwd()
    service.run_evaluation(predictions, output, instance_ids=["i2", "i1", "i2"], workers=2, namespace=None)
    assert len(calls) == 1
    cwd, kwargs = calls[0]
    assert cwd == output
    assert kwargs["instance_ids"] == ["i2", "i1"]
    assert kwargs["predictions_path"] == str(predictions)
    assert kwargs["max_workers"] == 2
    assert kwargs["namespace"] is None
    assert environment_calls == ["patch", "docker"]
    assert predictions.read_bytes() == original
    assert Path.cwd() == previous


@pytest.mark.parametrize("requested", [["missing"], ["empty"], ["i1", "missing"], [], [""], ["  "], [None]])
def test_invalid_selection_precedes_harness_and_environment(evaluation_env, requested):
    predictions, output, calls, environment_calls = evaluation_env
    with pytest.raises(ValueError, match="[Ii]nstance|predictions"):
        service.run_evaluation(predictions, output, instance_ids=requested)
    assert calls == []
    assert environment_calls == []
    assert not output.exists()


def test_cli_selects_comma_separated_ids(evaluation_env):
    predictions, output, calls, _ = evaluation_env
    result = CliRunner().invoke(
        app,
        [
            "evaluate",
            "--predictions",
            str(predictions),
            "--output",
            str(output),
            "--instance-id",
            " i2, i1,i2 ",
            "--namespace",
            "none",
            "--run-id",
            "selected",
        ],
    )
    assert result.exit_code == 0, result.output
    assert calls[0][1]["instance_ids"] == ["i2", "i1"]
    assert calls[0][1]["run_id"] == "selected"
    assert calls[0][1]["namespace"] is None


@pytest.mark.parametrize("value", ["missing", "empty", "i1,missing", "", "i1,", ",i1"])
def test_cli_reports_invalid_selection_without_evaluation(evaluation_env, value):
    predictions, output, calls, environment_calls = evaluation_env
    result = CliRunner().invoke(
        app,
        [
            "evaluate",
            "--predictions",
            str(predictions),
            "--output",
            str(output),
            "-i",
            value,
        ],
    )
    assert result.exit_code == 1, result.output
    assert "instance" in result.output.lower() or "predictions" in result.output.lower()
    assert calls == []
    assert environment_calls == []


def test_default_evaluation_keeps_all_nonempty_predictions(evaluation_env):
    predictions, output, calls, _ = evaluation_env
    result = CliRunner().invoke(app, ["evaluate", "--predictions", str(predictions), "--output", str(output)])
    assert result.exit_code == 0, result.output
    assert calls[0][1]["instance_ids"] == ["i1", "i2"]
