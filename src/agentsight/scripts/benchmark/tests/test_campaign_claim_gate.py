"""Incomplete campaigns retain measurements without authorizing optimization claims."""

import copy
import json
from pathlib import Path

import pytest
from test_benchmark import aggregate_report, campaign_data, complete_summary
from test_campaign_matrix_evidence import complete_formal_evidence


@pytest.mark.parametrize("mode", ["ab_comparison", "aa_calibration"])
@pytest.mark.parametrize("missing", [None, "regression", "matrix", "recovery"])
def test_claim_permission_requires_complete_evidence(
    tmp_path: Path, mode: str, missing: str | None
) -> None:
    configuration = campaign_data(tmp_path)
    settings, items, capacities, recovery, faults, regression = complete_formal_evidence(1)
    for section, values in settings.items():
        configuration[section].update(values)
    for _, run in items:
        run["summary"] = complete_summary(run.get("qps", 100))
        if run["version"] == "optimized":
            run["summary"]["resources"]["cpu_pct"]["avg"] = 10
    if missing == "regression":
        regression["full"] = False
    elif missing == "matrix":
        items[0][1]["evaluation"]["verdict"] = "FAIL"
    elif missing == "recovery":
        recovery.clear()
    (tmp_path / "manifest.json").write_text(json.dumps({"frozen": {"comparison_mode": mode}}))
    original = copy.deepcopy(items)
    result = aggregate_report.write_final(
        tmp_path,
        configuration,
        aggregate_report.matrix_rows(items),
        capacities,
        items,
        recovery,
        faults,
        regression,
    )
    allowed = missing is None and mode == "ab_comparison"
    assert result["optimization_claim_allowed"] is allowed
    assert result["verdict"] == ("PASS" if missing is None else "INCONCLUSIVE")
    assert result["comparison"]["metrics"]["cpu_avg_pct"]["delta_pct"] == -50
    document = (tmp_path / "final-report.md").read_text()
    assert ("CPU 占用降低" in document) is allowed
    assert items == original
