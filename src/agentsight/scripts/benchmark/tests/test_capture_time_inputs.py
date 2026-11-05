"""Capture validation must reject nonfinite deadlines before polling receipts."""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path
from types import ModuleType

import pytest


def load_validator() -> ModuleType:
    """Load only the standalone validator, without benchmark orchestration."""
    path = Path(__file__).parents[1] / "single_run" / "validate_results.py"
    spec = importlib.util.spec_from_file_location("capture_time_validator", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def complete_receipt_args(directory: Path) -> list[str]:
    """Use complete synthetic receipts so the real poll returns on its first read."""
    load = directory / "load.jsonl"
    load.write_text(
        json.dumps(
            {
                "request_id": "fixture-one",
                "metric": "benchmark_http_success",
                "data": {"value": 1},
            }
        )
        + "\n",
        encoding="utf-8",
    )
    receipts = directory / "receipts.tsv"
    receipts.write_text("T\tfixture-one\t200\t20\t1\n", encoding="utf-8")
    return [
        "validate_results.py",
        "--load-results",
        str(load),
        "--ffi-events",
        str(receipts),
        "--expected-total-tokens",
        "20",
    ]


@pytest.mark.parametrize("option", ["wait-seconds", "poll-interval"])
@pytest.mark.parametrize("value", ["nan", "inf"])
def test_nonfinite_timing_is_rejected(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, option: str, value: str
) -> None:
    validator = load_validator()
    monkeypatch.setattr(sys, "argv", [*complete_receipt_args(tmp_path), f"--{option}={value}"])
    with pytest.raises(SystemExit, match="finite"):
        validator.main()


@pytest.mark.parametrize(("wait", "interval"), [("0", "0.5"), ("0.25", "0.125")])
def test_finite_timing_keeps_complete_receipt_validation(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    wait: str,
    interval: str,
) -> None:
    validator = load_validator()
    monkeypatch.setattr(
        sys,
        "argv",
        [*complete_receipt_args(tmp_path), f"--wait-seconds={wait}", f"--poll-interval={interval}"],
    )
    assert validator.main() == 0
    result = json.loads(capsys.readouterr().out)
    assert result["sent"] == result["matched"] == result["complete"] == 1
    assert result["token_accuracy"] == 1


@pytest.mark.parametrize(
    ("option", "value"),
    [
        ("wait-seconds", "-1"),
        ("wait-seconds", "-inf"),
        ("poll-interval", "0"),
        ("poll-interval", "-inf"),
    ],
)
def test_existing_invalid_timing_stays_rejected(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, option: str, value: str
) -> None:
    validator = load_validator()
    monkeypatch.setattr(sys, "argv", [*complete_receipt_args(tmp_path), f"--{option}={value}"])
    with pytest.raises(SystemExit):
        validator.main()
