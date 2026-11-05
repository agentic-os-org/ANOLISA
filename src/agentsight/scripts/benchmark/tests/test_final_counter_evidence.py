"""Final drain counters must not turn invalid evidence into zero drops."""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from test_resource_report import artifacts, collect_metrics, resource_report, write_counters


def ffi_artifacts(root: Path, label: str) -> Path:
    directory = artifacts(root, label, 25, 40)
    worker = directory / "worker-result.json"
    record = json.loads(worker.read_text())
    record["trace_returncode"] = 0
    worker.write_text(json.dumps(record))
    (directory / "clients-ready.json").write_text('{"request_ids":["preflight-0"]}')
    (directory / "ffi-consumer.json").write_text('{"completed":true,"llm_events":2}')
    write_counters(
        directory,
        [
            {"ring_buffer_dropped": "0", "channel_dropped": "0"},
            {"ring_buffer_dropped": "0", "channel_dropped": "0"},
            {"ring_buffer_dropped": "0", "channel_dropped": "0"},
        ],
    )
    return directory


def final_snapshot(directory: Path, ring: str = "0", channel: str = "0", ffi: str = "0") -> None:
    (directory / "internal-metrics.prom").write_text(
        f"agentsight_ring_buffer_dropped_total {ring}\n"
        f"agentsight_channel_dropped_total {channel}\n"
        f'agentsight_ffi_dropped_total{{reason="count"}} {ffi}\n'
        'agentsight_ffi_dropped_total{reason="bytes"} 0\n'
    )


@pytest.mark.parametrize("field", ["ring", "channel"])
@pytest.mark.parametrize("invalid", ["nan", "inf", "-1", "broken"])
def test_invalid_final_probe_counter_cannot_be_hidden_by_earlier_zeros(
    tmp_path: Path, field: str, invalid: str
) -> None:
    directory = ffi_artifacts(tmp_path, "run")
    final_snapshot(directory, **{field: invalid})
    assert not resource_report.report(tmp_path, {"run": {"mode": "ffi"}})
    summary = json.loads((tmp_path / "resource-summary.json").read_text())
    measured = summary["runs"]["run"]
    assert measured["capture"] == "FAIL"
    assert "invalid internal counter" in measured["resource_error"]
    assert not summary["valid"] and not summary["comparison"]


@pytest.mark.parametrize("missing", ["ffi", "file"])
def test_lazy_ffi_counter_registration_preserves_missing_metric_semantics(
    tmp_path: Path, missing: str
) -> None:
    directory = ffi_artifacts(tmp_path, "run")
    if missing == "ffi":
        (directory / "internal-metrics.prom").write_text("agentsight_channel_dropped_total 0\n")
    result = resource_report.summarize(directory, {"mode": "ffi"})
    # FfiEventSender registers the counter only when an event is dropped.
    # A missing counter in an otherwise usable snapshot means zero; a missing
    # snapshot itself still cannot validate the run.
    assert result["valid"] is (missing == "ffi")
    assert bool(result["ffi_error"]) is (missing == "file")
    assert result["ffi_dropped"] == 0


@pytest.mark.parametrize("values", [["-1", "1"], ["1", "-1"], ["0.5", "-0.5"]])
def test_invalid_ffi_bucket_cannot_cancel_a_real_drop(tmp_path: Path, values: list[str]) -> None:
    directory = ffi_artifacts(tmp_path, "run")
    (directory / "internal-metrics.prom").write_text(
        "agentsight_channel_dropped_total 0\n"
        + "".join(
            f'agentsight_ffi_dropped_total{{reason="bucket-{index}"}} {value}\n'
            for index, value in enumerate(values)
        )
    )
    parsed = collect_metrics.read_internal_metrics(directory / "internal-metrics.prom")
    assert resource_report.numeric(parsed["ffi_dropped"]) is None
    result = resource_report.summarize(directory, {"mode": "ffi"})
    assert not result["valid"] and result["ffi_error"]


def test_invalid_final_snapshot_suppresses_ab_claims_and_explains_failure(tmp_path: Path) -> None:
    before = ffi_artifacts(tmp_path, "before")
    after = ffi_artifacts(tmp_path, "after")
    final_snapshot(before)
    final_snapshot(after, channel="nan")
    assert not resource_report.report(
        tmp_path, {"before": {"mode": "ffi"}, "after": {"mode": "ffi"}}
    )
    summary = json.loads((tmp_path / "resource-summary.json").read_text())
    assert summary["runs"]["before"]["valid"]
    assert not summary["runs"]["after"]["valid"]
    assert not summary["comparison"]
    text = (tmp_path / "resource-report.txt").read_text()
    assert "channel_dropped" in text and "N/A (invalid)" in text


def test_zero_final_counters_keep_a_valid_comparison(tmp_path: Path) -> None:
    for label in ("before", "after"):
        final_snapshot(ffi_artifacts(tmp_path, label))
    assert resource_report.report(tmp_path, {"before": {"mode": "ffi"}, "after": {"mode": "ffi"}})
    summary = json.loads((tmp_path / "resource-summary.json").read_text())
    assert summary["comparison"]["cpu_average_pct"]["percentage_points"] == 0


def test_valid_labeled_buckets_still_sum_and_fail_on_a_late_drop(tmp_path: Path) -> None:
    directory = ffi_artifacts(tmp_path, "run")
    final_snapshot(directory, ffi="2")
    result = resource_report.summarize(directory, {"mode": "ffi"})
    assert not result["valid"] and result["ffi_dropped"] == 2
    assert "dropped events" in result["ffi_error"]


def test_cli_mode_without_ffi_metrics_retains_its_contract(tmp_path: Path) -> None:
    artifacts(tmp_path, "run", 25, 40)
    result = resource_report.summarize(tmp_path / "run", {})
    assert result["valid"]
    assert result["ffi_error"] is result["ffi_dropped"] is None
