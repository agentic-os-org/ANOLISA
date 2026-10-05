#!/usr/bin/env python3
"""Aggregate immutable AgentSight campaign artifacts into the required reports."""

from __future__ import annotations

import argparse
import csv
import json
import math
from collections import defaultdict
from collections.abc import Iterable
from pathlib import Path
from statistics import median
from typing import Any

import campaign
import campaign_evidence


def read_json(path: Path) -> dict[str, Any]:
    """Read one JSON object."""
    value = json.loads(path.read_text(encoding="utf-8"))
    return value if isinstance(value, dict) else {}


def nested(value: dict[str, Any], *keys: str) -> float | bool | None:
    """Read a finite numeric or boolean value from nested dictionaries."""
    current: Any = value
    for key in keys:
        if not isinstance(current, dict):
            return None
        current = current.get(key)
    if isinstance(current, bool):
        return current
    if isinstance(current, (int, float)) and math.isfinite(current):
        return float(current)
    return None


def display(value: Any, suffix: str = "") -> str:
    """Format missing and numeric values consistently."""
    if value is None:
        return "—"
    if isinstance(value, float):
        return f"{value:.2f}{suffix}"
    return f"{value}{suffix}"


def ratio_display(value: float | None) -> str:
    """Format a ratio as a percentage."""
    return display(value * 100 if value is not None else None, "%")


def summary_values(runs: Iterable[dict[str, Any]], *keys: str) -> list[float]:
    """Collect numeric summary values from run-result objects."""
    values = []
    for run in runs:
        value = nested(run.get("summary", {}), *keys)
        if isinstance(value, (int, float)) and not isinstance(value, bool):
            values.append(float(value))
    return values


def spread(values: list[float]) -> tuple[float | None, float | None, float | None]:
    """Return median, minimum, and maximum for repeated runs."""
    return (median(values), min(values), max(values)) if values else (None, None, None)


def delta_pct(baseline: float | None, optimized: float | None) -> float | None:
    """Return optimized-versus-baseline percent change."""
    if baseline in (None, 0) or optimized is None:
        return None
    return (optimized - baseline) / baseline * 100


def discover(results: Path) -> list[tuple[Path, dict[str, Any]]]:
    """Load every preserved formal run result."""
    return [
        (path, read_json(path))
        for path in sorted(results.glob("runs/**/run-result.json"))
    ]


def write_run_inventory(
    results: Path, items: list[tuple[Path, dict[str, Any]]]
) -> None:
    """Export every discovered formal run without modifying source evidence."""
    fields = (
        "scenario",
        "version",
        "label",
        "repetition",
        "qps",
        "duration_seconds",
        "harness_exit_code",
        "verdict",
        "missing_gates",
        "failed_gates",
        "result_path",
    )
    with (results / "run-inventory.csv").open(
        "w", encoding="utf-8", newline=""
    ) as handle:
        writer = csv.DictWriter(handle, fieldnames=fields)
        writer.writeheader()
        for path, run in sorted(
            items, key=lambda item: item[0].relative_to(results).as_posix()
        ):
            evaluation = run.get("evaluation") or {}
            row = {field: run.get(field, "") for field in fields[:7]}
            row.update(
                verdict=evaluation.get("verdict", ""),
                missing_gates=json.dumps(
                    evaluation.get("missing", []), ensure_ascii=False
                ),
                failed_gates=json.dumps(
                    evaluation.get("failed", []), ensure_ascii=False
                ),
                result_path=path.relative_to(results).as_posix(),
            )
            writer.writerow(row)


def matrix_rows(items: list[tuple[Path, dict[str, Any]]]) -> list[dict[str, Any]]:
    """Aggregate matrix repetitions by absolute QPS and version."""
    grouped: dict[tuple[int, str], list[dict[str, Any]]] = defaultdict(list)
    for _, run in items:
        if run.get("scenario") == "matrix":
            grouped[(int(run["qps"]), run["version"])].append(run)
    rows = []
    for (qps, version), runs in sorted(grouped.items()):
        row: dict[str, Any] = {
            "qps": qps,
            "version": version,
            "repetitions": len(runs),
            "pass_count": sum(
                run.get("evaluation", {}).get("verdict") == "PASS" for run in runs
            ),
        }
        fields = {
            "effective_qps": ("effective_qps",),
            "http_success_rate": ("http_success_rate",),
            "http_error_rate": ("http_error_rate",),
            "timeout_rate": ("timeout_rate",),
            "cpu_avg_pct": ("resources", "cpu_pct", "avg"),
            "cpu_p95_pct": ("resources", "cpu_pct", "p95"),
            "cpu_p99_pct": ("resources", "cpu_pct", "p99"),
            "cpu_max_pct": ("resources", "cpu_pct", "max"),
            "rss_first_mb": ("resources", "rss_mb", "first"),
            "rss_last_mb": ("resources", "rss_mb", "last"),
            "rss_avg_mb": ("resources", "rss_mb", "avg"),
            "rss_p99_mb": ("resources", "rss_mb", "p99"),
            "rss_max_mb": ("resources", "rss_mb", "max"),
            "rss_slope_mb_per_hour": ("resources", "rss_mb", "slope_per_hour"),
            "latency_p50_ms": ("latency_ms", "p50"),
            "latency_p95_ms": ("latency_ms", "p95"),
            "latency_p99_ms": ("latency_ms", "p99"),
            "latency_max_ms": ("latency_ms", "max"),
            "drop_rate": ("drop_rate",),
            "trace_match_rate": ("trace_match_rate",),
            "trace_completeness": ("trace_completeness",),
            "token_accuracy": ("token_accuracy",),
        }
        for name, keys in fields.items():
            middle, minimum, maximum = spread(summary_values(runs, *keys))
            row[name] = middle
            row[f"{name}_min"] = minimum
            row[f"{name}_max"] = maximum
        rows.append(row)
    by_qps = defaultdict(dict)
    for row in rows:
        by_qps[row["qps"]][row["version"]] = row
    for versions in by_qps.values():
        before = versions.get("baseline")
        after = versions.get("optimized")
        if not before or not after:
            continue
        for field in (
            "effective_qps",
            "http_success_rate",
            "http_error_rate",
            "timeout_rate",
            "cpu_avg_pct",
            "rss_avg_mb",
            "rss_p99_mb",
            "rss_max_mb",
            "latency_p99_ms",
            "drop_rate",
            "trace_match_rate",
            "token_accuracy",
        ):
            after[f"{field}_delta_pct"] = delta_pct(before[field], after[field])
        if (
            before["trace_completeness"] is not None
            and after["trace_completeness"] is not None
        ):
            after["trace_completeness_delta_pp"] = (
                after["trace_completeness"] - before["trace_completeness"]
            ) * 100
    return rows


def write_performance(results: Path, rows: list[dict[str, Any]]) -> None:
    """Write matrix CSV plus the primary human-readable comparison table."""
    csv_path = results / "performance-comparison.csv"
    fieldnames = sorted({key for row in rows for key in row}) if rows else ["status"]
    with csv_path.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=fieldnames)
        writer.writeheader()
        if rows:
            writer.writerows(rows)
        else:
            writer.writerow({"status": "not-executed"})
    lines = [
        "# AgentSight 相同 QPS 性能对比",
        "",
        (
            "| QPS | 版本 | 重复/通过 | Effective QPS | CPU Avg/P95/P99/Max | "
            "RSS First/Last/Avg/P99/Max | RSS slope | Latency P50/P95/P99/Max | "
            "HTTP error/timeout | Drop rate | Trace match/completeness | Token accuracy |"
        ),
        "| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for row in rows:
        lines.append(
            f"| {row['qps']} | {row['version']} | {row['repetitions']}/{row['pass_count']} | "
            f"{display(row['effective_qps'])} | {display(row['cpu_avg_pct'])}/"
            f"{display(row['cpu_p95_pct'])}/{display(row['cpu_p99_pct'])}/"
            f"{display(row['cpu_max_pct'])}% | {display(row['rss_first_mb'])}/"
            f"{display(row['rss_last_mb'])}/{display(row['rss_avg_mb'])}/"
            f"{display(row['rss_p99_mb'])}/{display(row['rss_max_mb'])} MB | "
            f"{display(row['rss_slope_mb_per_hour'], ' MB/h')} | "
            f"{display(row['latency_p50_ms'])}/{display(row['latency_p95_ms'])}/"
            f"{display(row['latency_p99_ms'])}/{display(row['latency_max_ms'])} ms | "
            f"{ratio_display(row['http_error_rate'])}/{ratio_display(row['timeout_rate'])} | "
            f"{ratio_display(row['drop_rate'])} | {ratio_display(row['trace_match_rate'])}/"
            f"{ratio_display(row['trace_completeness'])} | {ratio_display(row['token_accuracy'])} |"
        )
    if not rows:
        lines.append("| — | — | — | — | — | — | — | — | — | — | — | — |")
        lines.extend(["", "状态：未执行完整的 baseline/optimized 五档矩阵。"])
    (results / "performance-comparison.md").write_text(
        "\n".join(lines) + "\n", encoding="utf-8"
    )


def write_capacity(results: Path) -> dict[str, dict[str, Any]]:
    """Write confirmed sustainable capacities and preserve missing results."""
    capacities = {}
    lines = [
        "# AgentSight 最大可持续 QPS",
        "",
        (
            "| 版本 | 预试验估算 QPS | 正式搜索起点 | 搜索探针数 | 最大可持续 QPS | "
            "已确认下界 | 相邻首个失败 QPS | 边界确认 | 正式确认 |"
        ),
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- |",
    ]
    for version in ("baseline", "optimized"):
        path = results / f"capacity-{version}.json"
        value = read_json(path) if path.exists() else {}
        capacities[version] = value
        confirmation = value.get("confirmation") or {}
        pretest = value.get("pretest") or {}
        probes = value.get("probes") or {}
        probe_count = len(probes) if value else None
        lines.append(
            f"| {version} | {display(pretest.get('estimate_qps'))} | "
            f"{display(value.get('search_start_qps'))} | {display(probe_count)} | "
            f"{display(value.get('maximum_sustainable_qps'))} | "
            f"{display(value.get('confirmed_lower_bound_qps'))} | "
            f"{display(value.get('first_failed_qps'))} | "
            f"{value.get('boundary_confirmed', False)} | "
            f"{json.dumps(confirmation, ensure_ascii=False) if confirmation else '—'} |"
        )
    lines.extend(
        [
            "",
            (
                "预试验只用于估算范围，正式搜索从估算值的 80%（按 QPS 分辨率取整）"
                "开始并通过二分搜索收敛；若正式起点失败，会先验证预试验最后通过档。"
                "候选长确认失败时也会在已知区间二分，不会逐个 QPS 降档。容量结论仍以"
                "正式重复确认为准，RSS 长期斜率由四小时长稳测试判定。"
            ),
        ]
    )
    if any(not item.get("boundary_confirmed") for item in capacities.values()):
        lines.extend(
            ["", "结论：`INCONCLUSIVE`。两个版本均完成三次正式确认后才能声明容量。"]
        )
    (results / "capacity-report.md").write_text(
        "\n".join(lines) + "\n", encoding="utf-8"
    )
    return capacities


def scenario_runs(
    items: list[tuple[Path, dict[str, Any]]], scenario: str
) -> list[tuple[Path, dict[str, Any]]]:
    """Filter discovered results by scenario."""
    return [(path, run) for path, run in items if run.get("scenario") == scenario]


def write_soak(results: Path, items: list[tuple[Path, dict[str, Any]]]) -> None:
    """Write four-hour bounded-memory and resource-leak evidence."""
    lines = [
        "# AgentSight 4 小时长稳报告",
        "",
        (
            "| 版本 | 时长 | RSS First/Last/Avg/P99/Max | RSS slope/5m increase | "
            "FD First/Last/Max/Slope | Thread First/Last/Max/Slope | "
            "Socket First/Last/Max/Slope | Channel/Event/Pending/Cache max | "
            "Drop/Trace completeness | 结论 |"
        ),
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |",
    ]
    runs = scenario_runs(items, "soak")
    for _, run in runs:
        summary = run["summary"]
        lines.append(
            f"| {run['version']} | {run['duration_seconds']}s | "
            f"{display(nested(summary, 'resources', 'rss_mb', 'first'))}/"
            f"{display(nested(summary, 'resources', 'rss_mb', 'last'))}/"
            f"{display(nested(summary, 'resources', 'rss_mb', 'avg'))}/"
            f"{display(nested(summary, 'resources', 'rss_mb', 'p99'))}/"
            f"{display(nested(summary, 'resources', 'rss_mb', 'max'))} MB | "
            f"{display(nested(summary, 'resources', 'rss_mb', 'slope_per_hour'), ' MB/h')}/"
            f"{display(nested(summary, 'resources', 'rss_mb', 'rolling_5m_max_increase'), ' MB')} | "
            f"{display(nested(summary, 'resources', 'file_descriptors', 'first'))}/"
            f"{display(nested(summary, 'resources', 'file_descriptors', 'last'))}/"
            f"{display(nested(summary, 'resources', 'file_descriptors', 'max'))}/"
            f"{display(nested(summary, 'resources', 'file_descriptors', 'slope_per_hour'), '/h')} | "
            f"{display(nested(summary, 'resources', 'threads', 'first'))}/"
            f"{display(nested(summary, 'resources', 'threads', 'last'))}/"
            f"{display(nested(summary, 'resources', 'threads', 'max'))}/"
            f"{display(nested(summary, 'resources', 'threads', 'slope_per_hour'), '/h')} | "
            f"{display(nested(summary, 'resources', 'active_connections', 'first'))}/"
            f"{display(nested(summary, 'resources', 'active_connections', 'last'))}/"
            f"{display(nested(summary, 'resources', 'active_connections', 'max'))}/"
            f"{display(nested(summary, 'resources', 'active_connections', 'slope_per_hour'), '/h')} | "
            f"{display(nested(summary, 'resources', 'channel_length', 'max'))}/"
            f"{display(nested(summary, 'resources', 'event_channel_bytes', 'max'))}/"
            f"{display(nested(summary, 'resources', 'pending_genai_bytes', 'max'))}/"
            f"{display(nested(summary, 'resources', 'connection_cache_bytes', 'max'))} | "
            f"{ratio_display(nested(summary, 'drop_rate'))}/"
            f"{ratio_display(nested(summary, 'trace_completeness'))} | "
            f"{run['evaluation']['verdict']} |"
        )
    if not runs:
        lines.append("| — | — | — | — | — | — | — | — | — | NOT EXECUTED |")
    (results / "soak-report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")


def write_recovery(
    results: Path,
    items: list[tuple[Path, dict[str, Any]]],
    settings: dict[str, Any],
    thresholds: dict[str, float],
) -> dict[tuple[str, int], dict[str, Any]]:
    """Write overload peaks and every required recovery-time gate."""
    grouped: dict[tuple[str, int], dict[str, tuple[Path, dict[str, Any]]]] = (
        defaultdict(dict)
    )
    for path, run in scenario_runs(items, "recovery"):
        grouped[(run["version"], run["repetition"])][run["label"]] = (path, run)
    lines = [
        "# AgentSight 过载恢复报告",
        "",
        (
            "| 版本/重复 | Overload RSS Max | QPS/P99/RSS recovery | Queue/cache recovery | "
            "Recovery completeness | 结论 |"
        ),
        "| --- | ---: | ---: | ---: | ---: | --- |",
    ]
    outcomes: dict[tuple[str, int], dict[str, Any]] = {}
    for (version, repetition), phases in sorted(grouped.items()):
        overload = phases.get("overload")
        recover = phases.get("recover")
        outcome = campaign_evidence.recovery_outcome(phases, settings, thresholds)
        outcomes[(version, repetition)] = outcome
        seconds = outcome["seconds"]
        lines.append(
            f"| {version}/{repetition} | "
            f"{display(nested(overload[1]['summary'], 'resources', 'rss_mb', 'max') if overload else None, ' MB')} | "
            f"{display(seconds.get('effective_qps'), 's')}/"
            f"{display(seconds.get('latency_p99_ms'), 's')}/"
            f"{display(seconds.get('rss_mb'), 's')} | "
            f"{display(seconds.get('channel_length'), 's')}/"
            f"{display(seconds.get('connection_cache_bytes'), 's')} | "
            f"{ratio_display(nested(recover[1]['summary'], 'trace_completeness') if recover else None)} | "
            f"{outcome['verdict']} |"
        )
    if not grouped:
        lines.append("| — | — | — | — | — | NOT EXECUTED |")
    (results / "recovery-report.md").write_text(
        "\n".join(lines) + "\n", encoding="utf-8"
    )
    return outcomes


def write_fault(
    results: Path,
    items: list[tuple[Path, dict[str, Any]]],
    settings: dict[str, Any],
    thresholds: dict[str, float],
) -> dict[str, dict[str, Any]]:
    """Write malformed-input outcomes and process survival evidence."""
    lines = [
        "# AgentSight 异常输入报告",
        "",
        "| 版本 | 类型 | 注入数 | Server healthy | AgentSight survived | 正常流量成功率/完整率 | 结论 |",
        "| --- | --- | ---: | --- | --- | ---: | --- |",
    ]
    found = False
    results_by_version: dict[str, dict[str, Any]] = {}
    for run_path, run in scenario_runs(items, "fault"):
        path = run_path.parent / "measurement" / "fault-results.json"
        if not path.exists():
            continue
        found = True
        fault = read_json(path)
        outcome = campaign_evidence.fault_outcome(run_path, run, settings, thresholds)
        results_by_version[run["version"]] = outcome
        summary = run.get("summary", {})
        for name, outcomes in fault.get("outcomes", {}).items():
            lines.append(
                f"| {run['version']} | {name} | {sum(outcomes.values())} | "
                f"{fault.get('server_healthy_after')} | {fault.get('process_alive_after')} | "
                f"{ratio_display(nested(summary, 'http_success_rate'))}/"
                f"{ratio_display(nested(summary, 'trace_completeness'))} | "
                f"{outcome['verdict']} |"
            )
    if not found:
        lines.append("| — | — | — | — | — | — | NOT EXECUTED |")
    (results / "fault-report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    return results_by_version


def write_regression(results: Path) -> dict[str, Any]:
    """Render externally captured regression commands and coverage gates."""
    path = results / "regression.json"
    regression = read_json(path) if path.exists() else {}
    checks = regression.get("checks", [])
    lines = [
        "# AgentSight 回归测试报告",
        "",
        "| 检查 | 退出码 | 结果 |",
        "| --- | ---: | --- |",
    ]
    for check in checks:
        lines.append(
            f"| `{check['command']}` | {check['exit_code']} | "
            f"{'PASS' if check['exit_code'] == 0 else 'FAIL'} |"
        )
    if not checks:
        lines.append("| — | — | NOT EXECUTED |")
    (results / "regression-report.md").write_text(
        "\n".join(lines) + "\n", encoding="utf-8"
    )
    return regression


def paired_matrix_rows(
    rows: list[dict[str, Any]],
) -> list[tuple[int, dict[str, Any], dict[str, Any]]]:
    """Return QPS levels containing both baseline and optimized measurements."""
    grouped: dict[int, dict[str, dict[str, Any]]] = defaultdict(dict)
    for row in rows:
        grouped[int(row["qps"])][str(row["version"])] = row
    return [
        (qps, versions["baseline"], versions["optimized"])
        for qps, versions in sorted(grouped.items())
        if "baseline" in versions and "optimized" in versions
    ]


def signed(value: float | None, suffix: str = "%") -> str:
    """Format a signed comparison while preserving unavailable evidence."""
    return "—" if value is None else f"{value:+.2f}{suffix}"


def comparison_summary(
    rows: list[dict[str, Any]],
    capacities: dict[str, dict[str, Any]],
    items: list[tuple[Path, dict[str, Any]]],
) -> dict[str, Any]:
    """Build machine-readable headline deltas for the final report."""
    pairs = paired_matrix_rows(rows)
    highest, before, after = pairs[-1] if pairs else (None, None, None)

    def values(field: str) -> tuple[float | None, float | None]:
        if before is None or after is None:
            return None, None
        baseline = before.get(field)
        optimized = after.get(field)
        return (
            float(baseline) if isinstance(baseline, (int, float)) else None,
            float(optimized) if isinstance(optimized, (int, float)) else None,
        )

    def regular_metric(field: str) -> dict[str, float | None]:
        baseline, optimized = values(field)
        return {
            "baseline": baseline,
            "optimized": optimized,
            "delta_pct": delta_pct(baseline, optimized),
        }

    trace_baseline, trace_optimized = values("trace_completeness")
    drop_baseline, drop_optimized = values("drop_rate")
    capacity_baseline = capacities.get("baseline", {}).get("maximum_sustainable_qps")
    capacity_optimized = capacities.get("optimized", {}).get("maximum_sustainable_qps")
    capacity_before = (
        float(capacity_baseline)
        if isinstance(capacity_baseline, (int, float))
        else None
    )
    capacity_after = (
        float(capacity_optimized)
        if isinstance(capacity_optimized, (int, float))
        else None
    )
    soak_slopes: dict[str, float | None] = {}
    for version in ("baseline", "optimized"):
        runs = [
            run
            for _, run in scenario_runs(items, "soak")
            if run.get("version") == version
        ]
        soak_slopes[version] = spread(
            summary_values(runs, "resources", "rss_mb", "slope_per_hour")
        )[0]
    return {
        "highest_common_qps": highest,
        "metrics": {
            "maximum_sustainable_qps": {
                "baseline": capacity_before,
                "optimized": capacity_after,
                "delta_pct": delta_pct(capacity_before, capacity_after),
            },
            "cpu_avg_pct": regular_metric("cpu_avg_pct"),
            "rss_avg_mb": regular_metric("rss_avg_mb"),
            "latency_p99_ms": regular_metric("latency_p99_ms"),
            "trace_completeness": {
                "baseline": trace_baseline,
                "optimized": trace_optimized,
                "delta_percentage_points": (
                    (trace_optimized - trace_baseline) * 100
                    if trace_baseline is not None and trace_optimized is not None
                    else None
                ),
            },
            "drop_rate": {
                "baseline": drop_baseline,
                "optimized": drop_optimized,
                "delta_pct": delta_pct(drop_baseline, drop_optimized),
                "delta_percentage_points": (
                    (drop_optimized - drop_baseline) * 100
                    if drop_baseline is not None and drop_optimized is not None
                    else None
                ),
            },
            "soak_rss_slope_mb_per_hour": {
                "baseline": soak_slopes["baseline"],
                "optimized": soak_slopes["optimized"],
                "delta_pct": delta_pct(
                    soak_slopes["baseline"], soak_slopes["optimized"]
                ),
            },
        },
    }


def change_conclusion(
    delta: float | None, subject: str, *, lower_is_better: bool
) -> str:
    """Describe whether a signed delta is an improvement or regression."""
    if delta is None:
        return "证据不足"
    if math.isclose(delta, 0.0, abs_tol=0.005):
        return f"{subject}基本不变"
    improved = delta < 0 if lower_is_better else delta > 0
    direction = "降低" if delta < 0 else "提高"
    return f"{subject}{direction}" if improved else f"{subject}{direction}（回退）"


def headline_lines(
    comparison: dict[str, Any], *, aa_calibration: bool = False
) -> list[str]:
    """Render the approved human-readable improvement summary."""

    def conclusion(delta: float | None, subject: str, *, lower_is_better: bool) -> str:
        if aa_calibration:
            return "A/A 测量波动" if delta is not None else "证据不足"
        return change_conclusion(delta, subject, lower_is_better=lower_is_better)

    highest = comparison["highest_common_qps"]
    metrics = comparison["metrics"]
    capacity = metrics["maximum_sustainable_qps"]
    cpu = metrics["cpu_avg_pct"]
    rss = metrics["rss_avg_mb"]
    latency = metrics["latency_p99_ms"]
    trace = metrics["trace_completeness"]
    drop = metrics["drop_rate"]
    soak = metrics["soak_rss_slope_mb_per_hour"]
    qps_label = f"相同 {highest} QPS" if highest is not None else "最高共同 QPS"
    rss_absolute = (
        rss["optimized"] - rss["baseline"]
        if rss["baseline"] is not None and rss["optimized"] is not None
        else None
    )
    rss_absolute_text = (
        f"（{signed(rss_absolute, ' MB')}）" if rss_absolute is not None else ""
    )
    return [
        "| 指标 | Baseline | Optimized | 变化 | 结论 |",
        "| --- | ---: | ---: | ---: | --- |",
        (
            f"| 最大可持续 QPS | {display(capacity['baseline'])} | "
            f"{display(capacity['optimized'])} | {signed(capacity['delta_pct'])} | "
            f"{conclusion(capacity['delta_pct'], '吞吐能力', lower_is_better=False)} |"
        ),
        (
            f"| {qps_label} 平均 CPU | {display(cpu['baseline'], '%')} | "
            f"{display(cpu['optimized'], '%')} | {signed(cpu['delta_pct'])} | "
            f"{conclusion(cpu['delta_pct'], 'CPU 占用', lower_is_better=True)} |"
        ),
        (
            f"| {qps_label} 平均 RSS | {display(rss['baseline'], ' MB')} | "
            f"{display(rss['optimized'], ' MB')} | {signed(rss['delta_pct'])}"
            f"{rss_absolute_text} | "
            f"{conclusion(rss['delta_pct'], '内存占用', lower_is_better=True)} |"
        ),
        (
            f"| {qps_label} P99 延迟 | {display(latency['baseline'], ' ms')} | "
            f"{display(latency['optimized'], ' ms')} | {signed(latency['delta_pct'])} | "
            f"{conclusion(latency['delta_pct'], '尾延迟', lower_is_better=True)} |"
        ),
        (
            f"| Trace 完整率 | {ratio_display(trace['baseline'])} | "
            f"{ratio_display(trace['optimized'])} | "
            f"{signed(trace['delta_percentage_points'], ' 个百分点')} | "
            f"{conclusion(trace['delta_percentage_points'], '完整率', lower_is_better=False)} "
            "|"
        ),
        (
            f"| 端到端采集丢失率 | {ratio_display(drop['baseline'])} | "
            f"{ratio_display(drop['optimized'])} | "
            f"{signed(drop['delta_percentage_points'], ' 个百分点')} | "
            f"{conclusion(drop['delta_percentage_points'], '采集丢失率', lower_is_better=True)} "
            "|"
        ),
        (
            f"| 4 小时 RSS 增长斜率 | {display(soak['baseline'], ' MB/h')} | "
            f"{display(soak['optimized'], ' MB/h')} | {signed(soak['delta_pct'])} | "
            f"{conclusion(soak['delta_pct'], '长期内存增长', lower_is_better=True)} |"
        ),
    ]


def matrix_comparison_lines(rows: list[dict[str, Any]]) -> list[str]:
    """Render compact per-QPS CPU, RSS, latency, and completeness deltas."""
    lines = [
        (
            "| QPS | CPU Baseline/Optimized/变化 | RSS Baseline/Optimized/变化 | "
            "P99 Baseline/Optimized/变化 | Trace Baseline/Optimized/差值 |"
        ),
        "| ---: | ---: | ---: | ---: | ---: |",
    ]
    for qps, baseline, optimized in paired_matrix_rows(rows):
        trace_delta = (
            (optimized["trace_completeness"] - baseline["trace_completeness"]) * 100
            if baseline["trace_completeness"] is not None
            and optimized["trace_completeness"] is not None
            else None
        )
        lines.append(
            f"| {qps} | {display(baseline['cpu_avg_pct'], '%')}/"
            f"{display(optimized['cpu_avg_pct'], '%')}/"
            f"{signed(optimized.get('cpu_avg_pct_delta_pct'))} | "
            f"{display(baseline['rss_avg_mb'], ' MB')}/"
            f"{display(optimized['rss_avg_mb'], ' MB')}/"
            f"{signed(optimized.get('rss_avg_mb_delta_pct'))} | "
            f"{display(baseline['latency_p99_ms'], ' ms')}/"
            f"{display(optimized['latency_p99_ms'], ' ms')}/"
            f"{signed(optimized.get('latency_p99_ms_delta_pct'))} | "
            f"{ratio_display(baseline['trace_completeness'])}/"
            f"{ratio_display(optimized['trace_completeness'])}/"
            f"{signed(trace_delta, ' 个百分点')} |"
        )
    if len(lines) == 2:
        lines.append("| — | — | — | — | — |")
    return lines


def write_final(
    results: Path,
    campaign_data: dict[str, Any],
    rows: list[dict[str, Any]],
    capacities: dict[str, dict[str, Any]],
    items: list[tuple[Path, dict[str, Any]]],
    recovery: dict[tuple[str, int], dict[str, Any]],
    faults: dict[str, dict[str, Any]],
    regression: dict[str, Any],
) -> dict[str, Any]:
    """Write the campaign-level final verdict and reproducibility pointers."""
    issues = campaign_evidence.audit_campaign(
        campaign_data, items, capacities, recovery, faults, regression
    )
    complete = not issues
    manifest = read_json(results / "manifest.json")
    host = manifest.get("host", {})
    frozen = manifest.get("frozen", {})
    frozen_versions = frozen.get("versions", {})
    comparison_mode = frozen.get("comparison_mode", "ab_comparison")
    aa_calibration = comparison_mode == "aa_calibration"
    comparison = comparison_summary(rows, capacities, items)
    capacity_lines = [
        (
            "| 版本 | 预试验估算 QPS | 正式搜索起点 | 最大可持续 QPS | "
            "相邻首个失败 QPS | 边界确认 | 变化 |"
        ),
        "| --- | ---: | ---: | ---: | ---: | --- | ---: |",
    ]
    capacity_delta = comparison["metrics"]["maximum_sustainable_qps"]["delta_pct"]
    for version in ("baseline", "optimized"):
        capacity = capacities.get(version, {})
        pretest = capacity.get("pretest") or {}
        capacity_lines.append(
            f"| {version} | {display(pretest.get('estimate_qps'))} | "
            f"{display(capacity.get('search_start_qps'))} | "
            f"{display(capacity.get('maximum_sustainable_qps'))} | "
            f"{display(capacity.get('first_failed_qps'))} | "
            f"{capacity.get('boundary_confirmed', False)} | "
            f"{signed(capacity_delta) if version == 'optimized' else '—'} |"
        )
    version_lines = [
        "| 版本 | Git commit | 二进制 SHA-256 | 配置 SHA-256 |",
        "| --- | --- | --- | --- |",
    ]
    for version in ("baseline", "optimized"):
        frozen = frozen_versions.get(version, {})
        commit = frozen.get("commit") or campaign_data["versions"][version].get(
            "commit"
        )
        version_lines.append(
            f"| {version} | `{commit or '—'}` | "
            f"`{frozen.get('binary_sha256') or '—'}` | "
            f"`{frozen.get('config_sha256') or '—'}` |"
        )
    regression_checks = regression.get("checks", [])
    regression_passed = sum(
        check.get("exit_code") == 0
        for check in regression_checks
        if isinstance(check, dict)
    )
    lines = [
        (
            "# AgentSight A/A 全量校准报告"
            if aa_calibration
            else "# AgentSight baseline/optimized 全量测试报告"
        ),
        "",
        "## 1. 最终结论",
        "",
        f"**总判定：{'PASS' if complete else 'INCONCLUSIVE'}**",
        "",
        (
            "**测试类型：A/A 测量稳定性校准。所有差值均视为测量波动，"
            "不得用于声明优化效果。**"
            if aa_calibration
            else "**测试类型：A/B 优化前后对比。**"
        ),
        "",
        (
            "该判定只覆盖 manifest 中冻结的二进制、配置、门槛和当前"
            "物理主机。"
            "未完成项目或缺失指标不会被推断为通过。"
        ),
        "",
        "### 核心波动摘要" if aa_calibration else "### 核心改进摘要",
        "",
        *headline_lines(comparison, aa_calibration=aa_calibration),
        "",
        (
            "> A/A 模式中 baseline 和 optimized 来自同一版本，表中变化用于"
            "量化测试噪声，不作改善或回退判断。"
            if aa_calibration
            else "> CPU、RSS、延迟和丢失率以降低为改善；最大可持续 QPS 和 "
            "Trace 完整率以提高为改善。`—` 表示证据不足，不能按零处理。"
        ),
        "",
        "## 2. 测试对象与环境",
        "",
        *version_lines,
        "",
        "| 环境项目 | 实际值 |",
        "| --- | --- |",
        f"| 主机 | {host.get('node', '—')} |",
        f"| 架构 | {host.get('machine', '—')} |",
        f"| Kernel | {host.get('kernel', '—')} |",
        f"| 内存 | {host.get('memory_total', '—')} |",
        f"| CPU governor | {host.get('cpu_governor', '—')} |",
        f"| BTF | {host.get('btf_available', False)} |",
        f"| k6 | {manifest.get('tools', {}).get('k6') or '—'} |",
        f"| OpenSSL | {manifest.get('tools', {}).get('openssl') or '—'} |",
        "",
        (
            f"本报告的性能结论只适用于以上 {host.get('machine', '未知架构')} "
            "主机环境，不能直接外推到其他架构或硬件。"
        ),
        "",
        "## 3. 最大可持续 QPS",
        "",
        (
            "最大可持续 QPS 必须由相邻失败点界定，并按冻结配置完成正式"
            "重复确认；"
            "未确认的边界显示为 `False`。"
        ),
        "",
        *capacity_lines,
        "",
        "## 4. 相同 QPS 性能对比",
        "",
        (
            "每个数值使用该 QPS 下重复运行的中位数。相同 QPS 对比可避免"
            "把处理更多"
            "请求误认为单请求资源开销降低。"
        ),
        "",
        *matrix_comparison_lines(rows),
        "",
        (
            "完整的 CPU P95/P99/Max、RSS First/Last/P99/Max、HTTP、Token 和延迟数据见 "
            "`performance-comparison.md` 与 `performance-comparison.csv`。"
        ),
        "",
        "## 5. 稳定性、恢复和异常输入",
        "",
        "- `soak-report.md`：4 小时 RSS、FD、线程、Socket 和 Trace 趋势。",
        "- `recovery-report.md`：过载后的 QPS、P99、RSS 和 queue/cache 恢复时间。",
        ("- `fault-report.md`：异常输入计数、服务健康和 AgentSight " "进程存活结果。"),
        "",
        "## 6. 回归测试与覆盖率",
        "",
        "| 项目 | 结果 |",
        "| --- | --- |",
        f"| 完整门禁 | {'是' if regression.get('full') else '否'} |",
        f"| 通过命令 | {regression_passed}/{len(regression_checks)} |",
        "| 详细日志 | `regression-report.md` 和 `regression.json` |",
        "",
        "## 7. 可追溯产物",
        "",
        "- `manifest.json`：冻结输入和主机元数据。",
        "- `final-summary.json`：机器可读的最终判定和核心性能差值。",
        "- `performance-comparison.csv/.md`：相同 QPS before/after 与 delta。",
        "- `capacity-report.md`、`soak-report.md`、`recovery-report.md`、`fault-report.md`。",
        "- `regression-report.md`：Cargo、pytest 与覆盖率门禁。",
        "- `runs/`：不可变的 JSONL、CSV、日志、SQLite 校验和单次报告。",
        "",
        "## 8. 风险和限制",
        "",
    ]
    if issues:
        lines.extend(f"- {issue}。" for issue in issues)
    else:
        lines.append("- 未发现阻断正式结论的缺失证据或失败门禁。")
    if manifest.get("tools", {}).get("h2load") is None:
        lines.append("- 未安装 h2load，因此没有可选的 HTTP/2 补充结果。")
    (results / "final-report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    summary = {
        "schema_version": 1,
        "verdict": "PASS" if complete else "INCONCLUSIVE",
        "comparison_mode": comparison_mode,
        "optimization_claim_allowed": not aa_calibration,
        "issues": issues,
        "comparison": comparison,
    }
    (results / "final-summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return summary


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--campaign", type=Path, required=True)
    parser.add_argument("--results", type=Path, required=True)
    args = parser.parse_args()
    campaign_data = campaign.read_json(args.campaign)
    campaign.validate_campaign(campaign_data)
    items = discover(args.results)
    write_run_inventory(args.results, items)
    rows = matrix_rows(items)
    write_performance(args.results, rows)
    capacities = write_capacity(args.results)
    write_soak(args.results, items)
    recovery = write_recovery(
        args.results,
        items,
        campaign_data["recovery"],
        campaign_data["thresholds"],
    )
    faults = write_fault(
        args.results,
        items,
        campaign_data["fault"],
        campaign_data["thresholds"],
    )
    regression = write_regression(args.results)
    write_final(
        args.results,
        campaign_data,
        rows,
        capacities,
        items,
        recovery,
        faults,
        regression,
    )
    print(f"final report: {args.results / 'final-report.md'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
