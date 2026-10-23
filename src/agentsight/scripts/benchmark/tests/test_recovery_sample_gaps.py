"""Missing observations cannot establish uninterrupted recovery."""

import gzip
import json
from pathlib import Path

import pytest
from test_benchmark import campaign_evidence


@pytest.mark.parametrize("compressed", [False, True])
def test_empty_request_seconds_break_recovery(tmp_path: Path, compressed: bool) -> None:
    run_path = tmp_path / "run-result.json"
    measurement = tmp_path / "measurement"
    measurement.mkdir()
    path = measurement / ("k6.jsonl.gz" if compressed else "k6.jsonl")
    rows = [
        {"metric": "benchmark_requests", "data": {"time": second, "value": 10}}
        for second in (0, 2, 4, 6)
    ]
    opener = gzip.open if compressed else open
    with opener(path, "wt", encoding="utf-8") as handle:
        handle.write("\n".join(json.dumps(row) for row in rows))
    samples = campaign_evidence.load_samples(run_path)["effective_qps"]
    assert campaign_evidence.continuous_recovery(samples, 10, 0.1, 4, higher_is_better=True) is None


def test_contiguous_request_seconds_keep_original_result(tmp_path: Path) -> None:
    measurement = tmp_path / "measurement"
    measurement.mkdir()
    rows = [
        {"metric": "benchmark_requests", "data": {"time": second, "value": 10}}
        for second in range(5)
    ]
    (measurement / "k6.jsonl").write_text("\n".join(json.dumps(row) for row in rows))
    samples = campaign_evidence.load_samples(tmp_path / "run-result.json")["effective_qps"]
    assert campaign_evidence.continuous_recovery(samples, 10, 0.1, 4, higher_is_better=True) == 0


def test_long_gap_uses_bounded_zero_evidence(tmp_path: Path) -> None:
    measurement = tmp_path / "measurement"
    measurement.mkdir()
    (measurement / "k6.jsonl").write_text(
        "\n".join(
            json.dumps({"metric": "benchmark_requests", "data": {"time": second, "value": 10}})
            for second in (0, 1000000000)
        )
    )
    samples = campaign_evidence.load_samples(tmp_path / "run-result.json")["effective_qps"]
    assert len(samples) == 3
    assert samples[1] == (1.0, 0.0)
