"""Regression tests for the benchmark HTML report renderers."""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

SINGLE_RUN = Path(__file__).parents[1] / "single_run"
REPO = Path(__file__).parents[5]
sys.path.insert(0, str(SINGLE_RUN))


def test_report_scripts_compile_on_runtime_interpreter() -> None:
    """Catch syntax that the CI-pinned Python rejects before rendering runs."""
    for name in ("plot_metrics.py", "plot_ab_comparison.py"):
        path = SINGLE_RUN / name
        compile(path.read_text(encoding="utf-8"), str(path), "exec")


def test_warmup_slope_excludes_future_samples() -> None:
    from plot_metrics import warmup_slope

    times = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0]
    values = [100.0, 100.0, 100.0, 100.0, 100.0, 200.0]
    _, rolling_times, rolling_values = warmup_slope(times, values, 0.0, 2.0)
    at_two = dict(zip(rolling_times, rolling_values))[2.0]
    assert at_two == pytest.approx(0.0, abs=1e-9)


def test_align_series_handles_unequal_and_irregular_timelines() -> None:
    from plot_ab_comparison import align_series

    grid, base, opt = align_series([0, 1, 2], [10, 20, 30], [0, 2], [100, 300])
    assert grid == [0, 1, 2]
    assert base == [10, 20, 30]
    assert opt == [100, 200, 300]

    grid, base, opt = align_series([0, 2], [100, 200], [0, 1, 2], [10, 20, 30])
    assert grid == [0, 1, 2]
    assert base == [100, 150, 200]
    assert opt == [10, 20, 30]

    grid, base, opt = align_series([0, 2, 5], [10, 30, 60], [1, 4], [20, 50])
    assert grid == [0, 1, 2, 4, 5]
    assert base == [10, 20, 30, 50, 60]
    assert opt == [None, 20, 30, 50, None]


def test_make_chart_handles_unequal_timelines() -> None:
    from plot_ab_comparison import make_chart

    baseline = {
        "metrics": {
            "times": [0, 1, 2],
            "columns": {"rss_mb": [100.0, 110.0, 120.0]},
        }
    }
    optimized = {
        "metrics": {
            "times": [0, 2],
            "columns": {"rss_mb": [90.0, 95.0]},
        }
    }
    svg = make_chart("ab-rss", "RSS", baseline, optimized, "rss_mb", "MiB", 350.0, 0.0, 700)
    assert "<svg" in svg


@pytest.mark.parametrize(
    ("key", "opt", "expected"),
    [
        ("drop_rate", 10.0, "轻微退化"),
        ("drop_rate", -1.0, "改善"),
        ("drop_rate", 0.0, "持平"),
        ("qps", 10.0, "改善"),
        ("qps", -1.0, "轻微退化"),
        ("qps", 0.0, "持平"),
    ],
)
def test_zero_baseline_verdict_uses_direction(key: str, opt: float, expected: str) -> None:
    from plot_ab_comparison import delta_and_verdict

    assert delta_and_verdict(key, 0.0, opt)[1] == expected


def test_ci_artifact_contract() -> None:
    from check_resource_limits import EXPECTED_SETTINGS

    workflow = (REPO / ".github/workflows/ci.yaml").read_text(encoding="utf-8")
    assert "run/metrics-report.html" in workflow
    assert "Warn on missing resource HTML report" in workflow
    assert "::warning::resource HTML report missing" in workflow
    assert "if-no-files-found: warn" in workflow
    assert (
        "RESOURCE_OUTPUT: /tmp/agentsight-resource-"
        "${{ github.run_id }}-${{ github.run_attempt }}" in workflow
    )
    assert (
        "RESOURCE_DIAGNOSTICS: /tmp/agentsight-resource-log-tails-"
        "${{ github.run_id }}-${{ github.run_attempt }}" in workflow
    )
    assert 'echo "RESOURCE_OUTPUT=$RUNNER_TEMP' not in workflow
    resource_job = workflow.split("test-agentsight-resource:", 1)[1].split(
        "\n  test-tokenless:", 1
    )[0]
    assert "group: ${{ github.repository }}-agentsight-resource-kernel66" in resource_job
    assert "cancel-in-progress: false" in resource_job
    assert f"--duration {EXPECTED_SETTINGS['duration']}" in resource_job
    assert (
        f"--cpu-budget-percent {EXPECTED_SETTINGS['cpu_budget_percent']}"
        in resource_job
    )


def write_ab_run(root: Path, label: str, cpu: float) -> Path:
    directory = root / label
    directory.mkdir(parents=True)
    (directory / "run-summary.json").write_text(
        json.dumps(
            {
                "resources": {
                    "cpu_pct": {"avg": cpu, "p95": cpu, "max": cpu},
                    "rss_mb": {"avg": 100.0, "max": 100.0},
                    "file_descriptors": {"slope_per_hour": 0.0},
                    "threads": {"slope_per_hour": 0.0},
                },
                "latency_ms": {"p99": 0.0},
                "effective_qps": 100.0,
                "tokens_per_second": {"generated": 100.0},
                "drop_rate": 0.0,
            }
        )
    )
    (directory / "report.json").write_text(
        json.dumps(
            {
                "admitted_completeness": 1.0,
                "completeness_ratio": 1.0,
                "token_accuracy": 1.0,
            }
        )
    )
    (directory / "metrics.csv").write_text(
        "monotonic_timestamp,rss_mb,cpu_pct\n0,100,10\n1,100,10\n"
    )
    return directory


def render_ab(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, summary: dict | None
) -> str:
    import plot_ab_comparison

    baseline = write_ab_run(tmp_path, "before", 0.666)
    optimized = write_ab_run(tmp_path, "after", 0.333)
    output = tmp_path / "ab-comparison.html"
    argv = [
        "plot_ab_comparison.py",
        "--baseline-dir",
        str(baseline),
        "--optimized-dir",
        str(optimized),
        "--output",
        str(output),
        "--warmup-seconds",
        "0",
        "--slope-window-seconds",
        "1",
        "--rss-gate-mib",
        "300",
        "--rss-average-gate-mib",
        "265",
    ]
    if summary is not None:
        path = tmp_path / "resource-summary.json"
        path.write_text(json.dumps(summary))
        argv.extend(["--resource-summary", str(path)])
    monkeypatch.setattr(sys, "argv", argv)
    plot_ab_comparison.render(plot_ab_comparison.parse_args())
    return output.read_text(encoding="utf-8")


def test_ab_draws_ci_gate_lines(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    html = render_ab(tmp_path, monkeypatch, None)
    assert "门禁均值 265 MiB" in html
    assert "门禁峰值 300 MiB" in html
    assert "safety-stop 350 MiB" in html
    assert "资源门禁：平均 CPU 25%" in html
    assert "P95/峰值 CPU 为诊断指标" in html


def test_ab_uses_authoritative_resource_summary(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    html = render_ab(
        tmp_path,
        monkeypatch,
        {
            "valid": True,
            "comparison_order": ["before", "after"],
            "runs": {
                "before": {
                    "rss_peak_mib": 100.0,
                    "rss_average_mib": 90.0,
                    "cpu_average_pct": 0.5000267,
                    "cpu_p95_pct": 0.5,
                },
                "after": {
                    "rss_peak_mib": 36.8,
                    "rss_average_mib": 36.8,
                    "cpu_average_pct": 0.4989570,
                    "cpu_p95_pct": 0.5,
                },
            },
        },
    )
    assert 'data-validity="valid"' in html
    assert "-0.2%" in html
    assert "-50.0%" not in html


def test_ab_invalid_summary_suppresses_improvement(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    html = render_ab(
        tmp_path,
        monkeypatch,
        {
            "valid": False,
            "comparison_order": ["before", "after"],
            "runs": {
                "before": {"rss_peak_mib": 100.0, "cpu_average_pct": 0.666},
                "after": {"rss_peak_mib": 36.8, "cpu_average_pct": 0.333},
            },
        },
    )
    assert 'data-validity="invalid"' in html
    assert "INVALID" in html
    assert 'class="delta-good"' not in html
    assert ">改善<" not in html
    assert "N/A (invalid)" in html


def test_ab_missing_summary_is_unverified(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    html = render_ab(tmp_path, monkeypatch, None)
    assert 'data-validity="unknown"' in html
    assert 'class="delta-good"' not in html


def test_run_sh_passes_metrics_args_to_validator() -> None:
    script = (SINGLE_RUN / "run.sh").read_text(encoding="utf-8")
    block = script.split("start_validator()", 1)[1].split("\n}", 1)[0]
    assert "--metrics-file" in block
    assert "--metrics-baseline" in block
    assert "internal-metrics-baseline.prom" in block
