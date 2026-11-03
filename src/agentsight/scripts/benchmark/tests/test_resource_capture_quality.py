"""Malformed capture quality cannot authorize a resource improvement claim."""

import json
from pathlib import Path

import pytest
from test_resource_report import artifacts, resource_report


@pytest.mark.parametrize(
    "field,value",
    [
        ("sent", True),
        ("sent", "1"),
        ("sent", 1.5),
        ("sent", None),
        ("sent", {}),
        ("completeness_ratio", True),
        ("completeness_ratio", "1"),
        ("completeness_ratio", "broken"),
        ("completeness_ratio", float("inf")),
        ("completeness_ratio", float("nan")),
        ("completeness_ratio", 1.01),
        ("completeness_ratio", {}),
        ("completeness_ratio", None),
        ("token_accuracy", True),
        ("token_accuracy", "1"),
        ("token_accuracy", []),
        ("token_accuracy", float("inf")),
        ("token_accuracy", float("nan")),
        ("token_accuracy", 1.01),
        ("token_accuracy", 10**400),
    ],
)
def test_invalid_capture_fields_suppress_persisted_improvements(
    tmp_path: Path, field: str, value: object
) -> None:
    artifacts(tmp_path, "before", 80, 400)
    directory = artifacts(tmp_path, "after", 20, 250)
    path = directory / "report.json"
    capture = json.loads(path.read_text())
    capture[field] = value
    path.write_text(json.dumps(capture))
    assert not resource_report.report(tmp_path, {"before": {}, "after": {}})
    summary = json.loads((tmp_path / "resource-summary.json").read_text())
    assert summary["runs"]["before"]["valid"]
    assert summary["runs"]["after"]["capture"] == "FAIL"
    assert not summary["comparison"]
    json.dumps(summary, allow_nan=False)
    assert "INVALID" in (tmp_path / "resource-report.txt").read_text()


@pytest.mark.parametrize("capture", [None, [], "not an object", 1])
def test_nonobject_capture_artifact_is_failed_evidence(tmp_path: Path, capture: object) -> None:
    directory = artifacts(tmp_path, "run", 20, 250)
    (directory / "report.json").write_text(json.dumps(capture))
    measured = resource_report.summarize(directory, {})
    assert not measured["valid"]
    assert measured["capture"] == "FAIL"


@pytest.mark.parametrize("quality", [0.999, 1, 1.0])
def test_valid_quality_boundaries_still_authorize_comparison(
    tmp_path: Path, quality: float
) -> None:
    for label in ("before", "after"):
        directory = artifacts(tmp_path, label, 20, 250)
        (directory / "report.json").write_text(
            json.dumps(
                {
                    "sent": 1000,
                    "completeness_ratio": quality,
                    "token_accuracy": quality,
                }
            )
        )
    assert resource_report.report(tmp_path, {"before": {}, "after": {}})
    summary = json.loads((tmp_path / "resource-summary.json").read_text())
    assert summary["valid"] and summary["comparison"]


def test_missing_capture_artifact_remains_unverified(tmp_path: Path) -> None:
    directory = artifacts(tmp_path, "run", 20, 250)
    (directory / "report.json").unlink()
    measured = resource_report.summarize(directory, {})
    assert measured["capture"] == "UNVERIFIED"
    assert not measured["valid"]


@pytest.mark.parametrize("quality", [0, -0.1, 0.998])
def test_finite_below_threshold_quality_is_still_a_failure(tmp_path: Path, quality: float) -> None:
    directory = artifacts(tmp_path, "run", 20, 250)
    (directory / "report.json").write_text(
        json.dumps(
            {
                "sent": 1,
                "completeness_ratio": quality,
                "token_accuracy": 1,
            }
        )
    )
    measured = resource_report.summarize(directory, {})
    assert measured["capture"] == "FAIL"
    assert not measured["valid"]


def test_zero_requests_do_not_establish_capture_quality(tmp_path: Path) -> None:
    directory = artifacts(tmp_path, "run", 20, 250)
    (directory / "report.json").write_text(
        json.dumps(
            {
                "sent": 0,
                "completeness_ratio": 1,
                "token_accuracy": 1,
            }
        )
    )
    assert not resource_report.summarize(directory, {})["valid"]


@pytest.mark.parametrize("sent", ["1", None, {}, [], True, 1.5])
def test_invalid_capture_count_keeps_ffi_reports_readable(tmp_path: Path, sent: object) -> None:
    directory = artifacts(tmp_path, "run", 20, 250)
    capture = {"sent": sent, "completeness_ratio": 1, "token_accuracy": 1, "capture_source": "ffi"}
    (directory / "report.json").write_text(json.dumps(capture))
    (directory / "ffi-consumer.json").write_text(json.dumps({"completed": True, "llm_events": 1}))
    assert not resource_report.report(tmp_path, {"run": {"mode": "ffi"}})
    summary = json.loads((tmp_path / "resource-summary.json").read_text())
    assert summary["runs"]["run"]["capture"] == "FAIL"
    assert summary["runs"]["run"]["capture_source"] == "ffi"
    json.dumps(summary, allow_nan=False)
