#!/usr/bin/env python3
"""Render a benchmark metrics.csv as a self-contained HTML/SVG evaluation report.

The report is read-only: it consumes the CSV and optional run-summary.json /
report.json produced by the benchmark harness and writes one HTML file. It has no
third-party dependencies and embeds its own charts, hover layer, and table view.
"""

from __future__ import annotations

import argparse
import csv
import html
import json
import math
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Sequence

MIB = 1024.0 * 1024.0

# Validated categorical slots (see the repository data-viz palette reference).
SERIES = ("--series-1", "--series-2", "--series-3")


def json_for_html(value: Any) -> str:
    """Serialize JSON without allowing ``</script>`` to close the embedding element."""
    return json.dumps(value, separators=(",", ":")).replace("</", "<\\/")


@dataclass(frozen=True)
class MetricDef:
    """One scalar column rendered as its own small-multiple chart."""

    key: str
    title: str
    unit: str
    section: str
    scale: float = 1.0
    digits: int = 1
    reference: float | None = None
    reference_label: str | None = None
    trend: bool = False


@dataclass
class PlotSeries:
    """A scaled series ready for SVG rendering and hover JSON."""

    label: str
    unit: str
    color: str
    color_name: str
    values: list[float | None]
    digits: int = 1


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Render benchmark metrics.csv as a self-contained HTML report."
    )
    parser.add_argument("metrics", type=Path, help="metrics.csv produced by collect_metrics.py")
    parser.add_argument("--run-summary", type=Path, help="optional run-summary.json")
    parser.add_argument("--report-json", type=Path, help="optional capture validation report.json")
    parser.add_argument("--output", type=Path, help="output HTML (default: metrics-report.html)")
    parser.add_argument("--title", help="report title")
    parser.add_argument(
        "--max-rss-mb",
        type=float,
        help="deprecated alias for --rss-target-mb (used only when --rss-target-mb is omitted)",
    )
    parser.add_argument(
        "--rss-target-mb",
        type=float,
        help="RSS acceptance target used by the memory charts (default: 350)",
    )
    parser.add_argument(
        "--connection-capacity", type=int, help="draw a horizontal pending-connection limit"
    )
    parser.add_argument(
        "--max-points", type=int, default=700, help="per-series points kept in the SVG"
    )
    parser.add_argument(
        "--warmup-seconds",
        type=float,
        default=30.0,
        help="exclude startup samples before computing RSS slope (default: 30)",
    )
    parser.add_argument(
        "--slope-window-seconds",
        type=float,
        default=15.0,
        help="rolling RSS slope window (default: 15; long runs should use 1800+)",
    )
    args = parser.parse_args()
    if args.rss_target_mb is None:
        args.rss_target_mb = args.max_rss_mb if args.max_rss_mb is not None else 350.0
    base = args.metrics.parent
    if args.run_summary is None and (base / "run-summary.json").is_file():
        args.run_summary = base / "run-summary.json"
    if args.report_json is None and (base / "report.json").is_file():
        args.report_json = base / "report.json"
    if args.output is None:
        args.output = base / "metrics-report.html"
    if args.title is None:
        args.title = f"AgentSight 基准测试 · {base.name or 'run'}"
    return args


def load_json(path: Path | None) -> dict:
    if path is None or not path.is_file():
        return {}
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return value if isinstance(value, dict) else {}


def number(row: dict[str, str], key: str) -> float | None:
    raw = row.get(key)
    if raw is None or raw == "":
        return None
    try:
        value = float(raw)
    except ValueError:
        return None
    return value if math.isfinite(value) else None


def read_metrics(path: Path) -> tuple[list[str], list[float | None], dict[str, list[float | None]]]:
    """Return relative timestamps and every interesting numeric column."""
    try:
        with path.open(newline="", encoding="utf-8") as handle:
            rows = list(csv.DictReader(handle))
    except OSError as error:
        raise SystemExit(f"cannot read {path}: {error}") from error
    if not rows:
        raise SystemExit(f"{path} contains no samples")

    time_key = (
        "monotonic_timestamp"
        if any(row.get("monotonic_timestamp") for row in rows)
        else "timestamp"
    )
    raw_times = [number(row, time_key) for row in rows]
    first = next((value for value in raw_times if value is not None), None)
    times = [
        value - first if value is not None and first is not None else None for value in raw_times
    ]

    columns: dict[str, list[float | None]] = {}
    for key in rows[0]:
        values = [number(row, key) for row in rows]
        if any(value is not None for value in values):
            columns[key] = values
    return rows, times, columns


def metric_defs(
    columns: dict[str, list[float | None]],
    max_rss_mb: float | None,
    connection_capacity: int | None,
) -> list[MetricDef]:
    defs = [
        MetricDef("rss_mb", "常驻内存", "MiB", "内存", trend=True),
        MetricDef(
            "connection_cache_bytes",
            "连接缓存",
            "MiB",
            "内存",
            scale=MIB,
        ),
        MetricDef(
            "pending_connection_bytes",
            "在途连接字节",
            "MiB",
            "内存",
            scale=MIB,
        ),
        MetricDef("pending_genai_bytes", "待处理 GenAI 字节", "MiB", "内存", scale=MIB),
        MetricDef(
            "event_channel_bytes",
            "入口事件队列",
            "MiB",
            "背压",
            scale=MIB,
        ),
        MetricDef("channel_length", "入口队列槽位", "事件", "背压", digits=0),
        MetricDef(
            "pending_connection_count",
            "在途连接数",
            "连接",
            "背压",
            digits=0,
        ),
        MetricDef(
            "completion_queue_bytes",
            "完成队列",
            "MiB",
            "背压",
            scale=MIB,
        ),
        MetricDef("cpu_pct", "CPU", "%", "CPU 与资源"),
        MetricDef("threads", "线程数", "线程", "CPU 与资源", digits=0),
        MetricDef("file_descriptors", "文件描述符", "个", "CPU 与资源", digits=0),
    ]

    budget = first_value(columns.get("event_channel_budget_bytes"))
    if budget is not None and budget > 0:
        for index, item in enumerate(defs):
            if item.key == "event_channel_bytes":
                defs[index] = MetricDef(
                    item.key,
                    item.title,
                    item.unit,
                    item.section,
                    scale=item.scale,
                    digits=item.digits,
                    reference=budget / MIB,
                    reference_label=f"预算 {budget / MIB:.0f} MiB",
                )
    if max_rss_mb is not None:
        for index, item in enumerate(defs):
            if item.key == "rss_mb":
                defs[index] = MetricDef(
                    item.key,
                    item.title,
                    item.unit,
                    item.section,
                    scale=item.scale,
                    digits=item.digits,
                    trend=item.trend,
                    reference=max_rss_mb,
                    reference_label=f"目标 {max_rss_mb:.0f} MiB",
                )
    if connection_capacity is not None:
        for index, item in enumerate(defs):
            if item.key == "pending_connection_count":
                defs[index] = MetricDef(
                    item.key,
                    item.title,
                    item.unit,
                    item.section,
                    scale=item.scale,
                    digits=item.digits,
                    reference=float(connection_capacity),
                    reference_label=f"缓存上限 {connection_capacity}",
                )
    return [item for item in defs if item.key in columns]


def first_value(values: Sequence[float | None] | None) -> float | None:
    if not values:
        return None
    return next((value for value in values if value is not None), None)


def last_value(values: Sequence[float | None]) -> float | None:
    return next((value for value in reversed(values) if value is not None), None)


def percentile(sorted_values: Sequence[float], fraction: float) -> float:
    if not sorted_values:
        return 0.0
    if len(sorted_values) == 1:
        return sorted_values[0]
    position = (len(sorted_values) - 1) * fraction
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return sorted_values[lower]
    weight = position - lower
    return sorted_values[lower] * (1.0 - weight) + sorted_values[upper] * weight


def slope_per_hour(times: Sequence[float | None], values: Sequence[float | None]) -> float | None:
    points = [
        (time, value)
        for time, value in zip(times, values)
        if time is not None and value is not None
    ]
    if len(points) < 2:
        return None
    mean_x = sum(x for x, _ in points) / len(points)
    mean_y = sum(y for _, y in points) / len(points)
    variance = sum((x - mean_x) ** 2 for x, _ in points)
    if variance == 0:
        return None
    covariance = sum((x - mean_x) * (y - mean_y) for x, y in points)
    return covariance / variance * 3600.0


def stats(times: Sequence[float | None], values: Sequence[float | None]) -> dict[str, float | None]:
    present = sorted(value for value in values if value is not None)
    return {
        "first": first_value(values),
        "last": last_value(values),
        "min": present[0] if present else None,
        "max": present[-1] if present else None,
        "avg": sum(present) / len(present) if present else None,
        "p50": percentile(present, 0.50) if present else None,
        "p95": percentile(present, 0.95) if present else None,
        "p99": percentile(present, 0.99) if present else None,
        "slope": slope_per_hour(times, values),
    }


def derive_rate(
    times: Sequence[float | None], values: Sequence[float | None], window: int = 5
) -> list[float | None]:
    """Return a smoothed per-second derivative of a cumulative counter."""
    raw: list[float | None] = [None] * len(values)
    previous: tuple[float, float] | None = None
    for index, value in enumerate(values):
        time = times[index]
        if time is None or value is None:
            continue
        if previous is not None:
            delta_time = time - previous[0]
            delta_value = value - previous[1]
            if delta_time > 0 and delta_value >= 0:
                raw[index] = delta_value / delta_time
        previous = (time, value)
    smoothed: list[float | None] = [None] * len(raw)
    recent: list[float] = []
    for index, value in enumerate(raw):
        if value is None:
            continue
        recent.append(value)
        recent = recent[-window:]
        smoothed[index] = sum(recent) / len(recent)
    return smoothed


def downsample(
    times: Sequence[float | None],
    series: Sequence[Sequence[float | None]],
    max_points: int,
) -> list[int]:
    """Keep first/last and per-bucket extrema so spikes survive the reduction."""
    count = len(times)
    if count <= max_points:
        return list(range(count))
    step = max(1.0, count / max_points)
    keep: set[int] = {0, count - 1}
    start = 0
    while start < count:
        end = min(count, max(start + 1, int(round(start + step))))
        for values in series:
            window = [index for index in range(start, end) if values[index] is not None]
            if not window:
                continue
            keep.add(min(window, key=lambda index: values[index]))
            keep.add(max(window, key=lambda index: values[index]))
        start = end
    return sorted(keep)


def fmt_time(seconds: float) -> str:
    seconds = max(0.0, seconds)
    if seconds < 60:
        return f"{seconds:.0f}s"
    minutes, remainder = divmod(int(round(seconds)), 60)
    if minutes < 60:
        return f"{minutes}m{remainder:02d}s"
    hours, minutes = divmod(minutes, 60)
    return f"{hours}h{minutes:02d}m"


def fmt_value(value: float | None, unit: str, digits: int = 1) -> str:
    if value is None:
        return "—"
    text = f"{value:.{digits}f}"
    return f"{text} {unit}" if unit else text


def nice_ticks(maximum: float, count: int = 4) -> list[float]:
    if maximum <= 0:
        return [0.0, 1.0]
    raw = maximum / count
    magnitude = 10 ** math.floor(math.log10(raw))
    step = magnitude
    for candidate in (1.0, 2.0, 2.5, 5.0, 10.0):
        step = candidate * magnitude
        if step >= raw:
            break
    top = math.ceil(maximum / step) * step
    ticks: list[float] = []
    value = 0.0
    while value <= top + step * 0.001 and len(ticks) < 12:
        ticks.append(value)
        value += step
    return ticks


def svg_path(times: Sequence[float], values: Sequence[float | None], x_of, y_of) -> str:
    commands: list[str] = []
    pen_down = False
    for time, value in zip(times, values):
        if time is None or value is None:
            pen_down = False
            continue
        command = "L" if pen_down else "M"
        commands.append(f"{command}{x_of(time):.1f},{y_of(value):.1f}")
        pen_down = True
    return " ".join(commands)


def build_chart(
    chart_id: str,
    title: str,
    times: Sequence[float | None],
    plots: list[PlotSeries],
    reference: tuple[float, str] | None,
    trend: bool,
    max_points: int,
    width: int = 760,
    height: int = 250,
    warmup: float | None = None,
    trend_from: float | None = None,
) -> tuple[str, dict]:
    left, right, top, bottom = 64, 22, 18, 34
    plot_width = width - left - right
    plot_height = height - top - bottom

    indices = downsample(times, [plot.values for plot in plots], max_points)
    chart_times = [times[index] for index in indices]
    reduced = [
        PlotSeries(
            plot.label,
            plot.unit,
            plot.color,
            plot.color_name,
            [plot.values[index] for index in indices],
            plot.digits,
        )
        for plot in plots
    ]

    valid = [value for plot in reduced for value in plot.values if value is not None]
    data_max = max(valid) if valid else 0.0
    if reference is not None:
        data_max = max(data_max, reference[0])
    if data_max > 0:
        data_max *= 1.05
    ticks = nice_ticks(data_max)
    y_max = ticks[-1] if ticks[-1] > 0 else 1.0
    axis_digits = 0 if all(plot.digits == 0 for plot in reduced) else (0 if y_max >= 10 else 1)
    y_min = min(0.0, min(valid) if valid else 0.0)
    if y_min < 0:
        ticks = nice_ticks(max(abs(y_min), y_max))
        y_max = ticks[-1]
        y_min = -y_max

    valid_times = [time for time in chart_times if time is not None]
    x_min = min(valid_times) if valid_times else 0.0
    x_max = max(valid_times) if valid_times else 1.0
    if x_max <= x_min:
        x_max = x_min + 1.0

    def x_of(time: float) -> float:
        return left + (time - x_min) / (x_max - x_min) * plot_width

    def y_of(value: float) -> float:
        return top + (1.0 - (value - y_min) / (y_max - y_min)) * plot_height

    parts: list[str] = [
        f'<svg class="chart" data-chart="{chart_id}" viewBox="0 0 {width} {height}" '
        f'role="img" aria-labelledby="{chart_id}-title">',
        f'<title id="{chart_id}-title">{html.escape(title)}</title>',
        f'<desc>{html.escape(title)}随测量窗口的变化。</desc>',
    ]

    for tick in ticks:
        y = y_of(tick)
        parts.append(
            f'<line class="gridline" x1="{left}" y1="{y:.1f}" x2="{left + plot_width}" '
            f'y2="{y:.1f}"/>'
        )
        parts.append(
            f'<text class="axis" x="{left - 8}" y="{y + 4:.1f}" text-anchor="end">'
            f"{html.escape(fmt_value(tick, '', axis_digits))}</text>"
        )

    for index in range(5):
        fraction = index / 4
        time = x_min + (x_max - x_min) * fraction
        x = left + plot_width * fraction
        parts.append(
            f'<line class="gridline" x1="{x:.1f}" y1="{top}" x2="{x:.1f}" '
            f'y2="{top + plot_height}"/>'
        )
        parts.append(
            f'<text class="axis" x="{x:.1f}" y="{height - 12}" text-anchor="middle">'
            f"{html.escape(fmt_time(time))}</text>"
        )

    parts.append(
        f'<line class="baseline" x1="{left}" y1="{top + plot_height}" '
        f'x2="{left + plot_width}" y2="{top + plot_height}"/>'
    )

    if reference is not None:
        reference_value, reference_label = reference
        if y_min <= reference_value <= y_max:
            y = y_of(reference_value)
            parts.append(
                f'<line class="reference" x1="{left}" y1="{y:.1f}" '
                f'x2="{left + plot_width}" y2="{y:.1f}"/>'
            )
            parts.append(
                f'<text class="last-label" x="{left + 6}" y="{y - 6:.1f}">'
                f"{html.escape(reference_label)}</text>"
            )

    if warmup is not None and x_min <= warmup <= x_max:
        x = x_of(warmup)
        parts.append(
            f'<line x1="{x:.1f}" y1="{top}" x2="{x:.1f}" y2="{top + plot_height}" '
            'style="stroke: var(--series-2); stroke-width:1.5; stroke-dasharray:4 4; opacity:.85"/>'
        )
        parts.append(
            f'<text class="axis" x="{x + 6:.1f}" y="{top + 12}" '
            f'style="fill: var(--series-2)">warmup {html.escape(fmt_time(warmup))}</text>'
        )

    for plot in reduced:
        path = svg_path(chart_times, plot.values, x_of, y_of)
        if path:
            parts.append(
                f'<path class="series-line" style="stroke: var({plot.color_name})" d="{path}"/>'
            )

    root_colors = ["var(--text-secondary)"]
    labels: list[tuple[str, float, float, str]] = []
    for index, plot in enumerate(reduced):
        last_index = next(
            (idx for idx in range(len(plot.values) - 1, -1, -1) if plot.values[idx] is not None),
            None,
        )
        if last_index is None:
            continue
        value = plot.values[last_index]
        time = chart_times[last_index]
        if value is None or time is None:
            continue
        x, y = x_of(time), y_of(value)
        parts.append(
            f'<circle cx="{x:.1f}" cy="{y:.1f}" r="4.5" '
            f'style="fill: var({plot.color_name}); stroke: var(--surface-1); stroke-width: 2"/>'
        )
        color = root_colors[0]
        labels.append((fmt_value(value, plot.unit, plot.digits), x, y, color))
    labels.sort(key=lambda item: item[2])
    previous_y = -1000.0
    for text, x, y, _ in labels:
        label_y = max(top + 10, max(y - 8, previous_y + 14))
        previous_y = label_y
        parts.append(
            f'<text class="last-label" x="{x - 4:.1f}" y="{label_y:.1f}" text-anchor="end">'
            f"{html.escape(text)}</text>"
        )

    if trend and len(reduced) == 1:
        source_times = chart_times
        source_values = reduced[0].values
        if trend_from is not None:
            subset = [
                (time, value)
                for time, value in zip(chart_times, reduced[0].values)
                if time is not None and value is not None and time >= trend_from
            ]
            if len(subset) >= 2:
                source_times = [time for time, _ in subset]
                source_values = [value for _, value in subset]
        fit = slope_per_hour(source_times, source_values)
        first = first_value(source_values)
        first_time = next((time for time in source_times if time is not None), None)
        last_time = next((time for time in reversed(source_times) if time is not None), None)
        if fit is not None and first is not None and first_time is not None and last_time is not None:
            start_x = x_of(first_time)
            end_x = x_of(last_time)
            start_y = y_of(first)
            end_y = y_of(first + fit * (last_time - first_time) / 3600.0)
            parts.append(
                f'<line class="trend" x1="{start_x:.1f}" y1="{start_y:.1f}" '
                f'x2="{end_x:.1f}" y2="{end_y:.1f}"/>'
            )

    for index, plot in enumerate(reduced):
        parts.append(
            f'<circle class="hover-dot" data-series="{index}" r="4.5" '
            f'style="display:none; fill: var({plot.color_name}); '
            f'stroke: var(--surface-1); stroke-width: 2"/>'
        )
    parts.append(f'<line class="crosshair" style="display:none"/>')
    parts.append(
        f'<rect class="hit" x="{left}" y="{top}" width="{plot_width}" '
        f'height="{plot_height}"/>'
    )
    parts.append("</svg>")

    chart_data = {
        "t": [time for time in chart_times],
        "series": [
            {
                "label": plot.label,
                "unit": plot.unit,
                "color": plot.color_name,
                "digits": plot.digits,
                "v": plot.values,
            }
            for plot in reduced
        ],
        "plot": {"left": left, "top": top, "width": plot_width, "height": plot_height},
        "yMin": y_min,
        "yMax": y_max,
    }
    return "".join(parts), chart_data


def status_icon(kind: str) -> str:
    if kind == "good":
        return (
            '<svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="7" '
            'fill="currentColor"/><path d="M4.5 8.2l2.2 2.2 4.6-4.8" fill="none" '
            'stroke="#fff" stroke-width="1.8" stroke-linecap="round" '
            'stroke-linejoin="round"/></svg>'
        )
    return (
        '<svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="7" '
        'fill="currentColor"/><path d="M8 4.2v4.6" stroke="#fff" stroke-width="1.8" '
        'stroke-linecap="round"/><circle cx="8" cy="11.6" r="1" fill="#fff"/></svg>'
    )


def kpi(
    label: str,
    value: str,
    sub: str = "",
    status: str | None = None,
    status_label: str = "",
) -> str:
    status_html = ""
    if status is not None:
        status_html = (
            f'<span class="status status-{status}">{status_icon(status)}'
            f"<span>{html.escape(status_label)}</span></span>"
        )
    sub_html = f'<div class="sub">{status_html}{html.escape(sub)}</div>' if sub or status else ""
    return (
        f'<div class="kpi"><div class="label">{html.escape(label)}</div>'
        f'<div class="value">{html.escape(value)}</div>{sub_html}</div>'
    )


def counter_delta(values: Sequence[float | None] | None) -> float | None:
    if not values:
        return None
    first = first_value(values)
    last = last_value(values)
    if first is None or last is None:
        return None
    return max(0.0, last - first)


def read_text_file(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return ""


def scan_stability(run_dir: Path, worker: dict[str, Any]) -> dict[str, Any]:
    """Collect panic/OOM/restart evidence from the run artifacts."""
    logs = read_text_file(run_dir / "trace.log") + "\n" + read_text_file(run_dir / "runner.log")
    panic_count = len(
        re.findall(r"(?i)(thread '[^']*' panicked at|panicked at|panic!)", logs)
    )
    abort_count = len(re.findall(r"(?i)(SIGABRT|Aborted|core dumped)", logs))
    oom_recovery_none = "OOM recovery: no OOM kill events found in dmesg" in logs
    oom_lines = [
        line
        for line in logs.splitlines()
        if re.search(r"(?i)(out of memory|oom-kill|killed process)", line)
        and "no OOM kill events found" not in line
        and "OOM recovery: scanning" not in line
    ]
    oom_kill = 0 if oom_recovery_none else len(oom_lines)
    cgroup_events = run_dir / "memory.events"
    cgroup_oom: int | str = "N/A"
    if cgroup_events.is_file():
        values: dict[str, int] = {}
        for line in read_text_file(cgroup_events).splitlines():
            fields = line.split()
            if len(fields) == 2 and fields[0] in {"oom", "oom_kill"}:
                try:
                    values[fields[0]] = int(fields[1])
                except ValueError:
                    pass
        if values:
            cgroup_oom = values.get("oom_kill", 0)
    safety_stop = (run_dir / "safety-stop.json").is_file()
    status = worker.get("status")
    trace_returncode = worker.get("trace_returncode")
    restart_count: int | None
    if status is None and trace_returncode is None:
        restart_count = None
    else:
        restart_count = 0 if status == "complete" and trace_returncode in (0, None) else 1
    return {
        "panic": panic_count,
        "abort": abort_count,
        "oom": oom_kill,
        "cgroup_oom": cgroup_oom,
        "restarts": restart_count,
        "exit_code": trace_returncode if isinstance(trace_returncode, int) else worker.get("returncode"),
        "trace_survived": worker.get("trace_survived"),
        "safety_stop": safety_stop,
        "oom_note": (
            "AgentSight startup OOM recovery: dmesg 无 OOM kill"
            if oom_kill == 0
            else f"发现 {oom_kill} 条 OOM 记录"
        ),
    }


def stability_section(run_dir: Path, worker: dict[str, Any]) -> str:
    info = scan_stability(run_dir, worker)
    cgroup_value = (
        str(info["cgroup_oom"]) if isinstance(info["cgroup_oom"], int) else "N/A"
    )
    restarts = info["restarts"] if info["restarts"] is not None else "—"
    exit_code = info["exit_code"] if info["exit_code"] is not None else "—"
    survived = info["trace_survived"]
    survived_text = (
        str(survived).lower() if isinstance(survived, bool) else "—"
    )
    safety_text = "有" if info["safety_stop"] else "无"
    return f"""
<section id="stability"><h2>稳定性</h2>
<div class="kpis" style="margin-top: 0">
  {kpi("Panic", str(info["panic"]), info["oom_note"], "good" if info["panic"] == 0 else "critical", "正常" if info["panic"] == 0 else "发现 panic")}
  {kpi("OOM kill", str(info["oom"]), info["oom_note"], "good" if info["oom"] == 0 else "critical", "正常" if info["oom"] == 0 else "发现 OOM")}
  {kpi("进程重启", str(restarts), "worker-result.json", "good" if restarts == 0 else "warning", "无" if restarts == 0 else "有")}
  {kpi("安全停止", safety_text, "safety-stop.json", "good" if not info["safety_stop"] else "critical", "无" if not info["safety_stop"] else "有")}
  {kpi("cgroup OOM", cgroup_value, "memory.events", "good" if cgroup_value == "0" else "warning", "N/A" if cgroup_value == "N/A" else "已记录")}
</div>
<table><thead><tr><th>项目</th><th>值</th><th>来源</th><th>说明</th></tr></thead><tbody>
<tr><td>Panic 次数</td><td>{info["panic"]}</td><td><code>trace.log</code> / <code>runner.log</code></td><td>包含 thread panic；进程存活不代表 0</td></tr>
<tr><td>Abort / SIGABRT</td><td>{info["abort"]}</td><td>logs</td><td>进程级 abort / core dump</td></tr>
<tr><td>OOM kill</td><td>{info["oom"]}</td><td><code>trace.log</code> OOM recovery / dmesg</td><td>内核 OOM kill 次数</td></tr>
<tr><td>cgroup oom / oom_kill</td><td>{cgroup_value}</td><td><code>memory.events</code></td><td>本次未在 cgroup 中运行时为 N/A</td></tr>
<tr><td>进程重启</td><td>{restarts}</td><td><code>worker-result.json</code></td><td>status={worker.get("status", "—")}, returncode={worker.get("returncode", "—")}</td></tr>
<tr><td>AgentSight exit</td><td>{exit_code}</td><td><code>worker-result.json</code></td><td>trace_returncode</td></tr>
<tr><td>Trace survived</td><td>{survived_text}</td><td><code>worker-result.json</code></td><td>采集进程是否一直存活</td></tr>
<tr><td>安全停止</td><td>{safety_text}</td><td><code>safety-stop.json</code></td><td>内存/磁盘 guard 是否触发</td></tr>
</tbody></table>
<p class="sub">Panic 0 / OOM 0 只说明“没崩”；容错完成还要看 fault 注入期间正常流量、fault 分类记录、资源回落。</p>
</section>
"""


def fault_info(run_dir: Path) -> dict[str, Any]:
    return load_json(run_dir / "fault-results.json")


def fault_section(run_dir: Path) -> str:
    fault = fault_info(run_dir)
    known = [
        ("invalid_json", "JSON 截断/非法", "protocol_error"),
        ("invalid_chunk_size", "chunked 块大小非法", "protocol_error"),
        ("content_length_mismatch", "Content-Length 不匹配", "protocol_error"),
        ("truncated_body", "请求体截断", "protocol_error / incomplete"),
        ("truncated_sse", "SSE 中途断开", "incomplete / sse_truncated"),
        ("invalid_utf8", "非法 UTF-8", "protocol_error"),
        ("binary_body", "二进制 body", "protocol_error"),
        ("oversized_body", "超大 body", "over_budget / protocol_error"),
    ]
    outcomes = fault.get("outcomes") if isinstance(fault.get("outcomes"), dict) else {}
    rows: list[str] = []
    for name, _, expected in known:
        counts = outcomes.get(name) if isinstance(outcomes.get(name), dict) else {}
        total = sum(int(v) for v in counts.values() if isinstance(v, (int, float))) if counts else None
        actual = (
            "; ".join(f"{key}={value}" for key, value in counts.items())
            if counts
            else ("N/A" if not fault else "无记录")
        )
        rows.append(
            f"<tr><td><code>{html.escape(name)}</code></td><td>{total if total is not None else '—'}</td>"
            f"<td>{html.escape(expected)}</td><td>{html.escape(actual)}</td></tr>"
        )
    for name, counts in outcomes.items():
        if name in {item[0] for item in known} or not isinstance(counts, dict):
            continue
        total = sum(int(v) for v in counts.values() if isinstance(v, (int, float)))
        actual = "; ".join(f"{key}={value}" for key, value in counts.items())
        rows.append(
            f"<tr><td><code>{html.escape(str(name))}</code></td><td>{total}</td>"
            f"<td>—</td><td>{html.escape(actual)}</td></tr>"
        )
    if fault:
        normal = fault.get("normal_traffic") if isinstance(fault.get("normal_traffic"), dict) else {}
        completeness = normal.get("completeness_ratio", normal.get("completeness"))
        normal_text = (
            f"{completeness * 100:.1f}%" if isinstance(completeness, (int, float)) else "N/A"
        )
        fault_count = sum(
            sum(int(v) for v in counts.values() if isinstance(v, (int, float)))
            for counts in outcomes.values()
            if isinstance(counts, dict)
        )
        status_value = str(len(outcomes))
        status_sub = "已记录 fault 类型"
    else:
        normal_text = "N/A"
        fault_count = "—"
        status_value = "未运行"
        status_sub = "fault-results.json 不存在"
    return f"""
<section id="fault-injection"><h2>Fault 注入</h2>
<div class="kpis" style="margin-top: 0">
  {kpi("Fault 注入", status_value, status_sub, "good" if fault else "warning", "已运行" if fault else "未运行")}
  {kpi("Fault 请求数", str(fault_count), "本次 run")}
  {kpi("正常流量完整率", normal_text, "fault 窗口", "good" if normal_text != "N/A" else None, "")}
  {kpi("Fault 分类", str(len(outcomes)) if fault else "N/A", "fault-results.json")}
</div>
<table><thead><tr><th>Fault 类型</th><th>次数</th><th>期望分类</th><th>实际结果</th></tr></thead><tbody>{''.join(rows)}</tbody></table>
<p class="sub">本次 run {'没有运行 fault_agent/fault_injector' if not fault else '已读取 fault-results.json'}；fault 窗口内正常流量需要单独统计。</p>
</section>
"""


def runtime_limits_section(run_dir: Path) -> str:
    config = load_json(run_dir / "config.json")
    limits = config.get("runtime_limits") if isinstance(config.get("runtime_limits"), dict) else {}
    if not limits:
        return ""
    notes = {
        "cpu_budget_percent": "应用层协作 CPU 上限；0 关闭，cgroup 才是硬限制",
        "ring_buffer_mb": "eBPF ring buffer 总大小",
        "event_channel_capacity": "Probe → 事件循环的队列槽位",
        "event_channel_policy": "队列满策略：backpressure 阻塞消费；可选 drop_newest / sample:N",
        "event_channel_max_bytes_mb": "入口事件队列字节预算",
        "pending_genai_max_count": "等待 session_id 的 GenAI 事件数上限",
        "pending_genai_max_bytes_mb": "等待 session_id 的 GenAI 字节预算",
        "pid_cache_size": "PID → agent_name LRU 条目",
        "connection_capacity": "HTTP 连接/流槽位；应 ≥ 并发 agent 数",
        "connection_max_bytes_mb": "HTTP/1 + HTTP/2 连接缓存总字节预算",
        "max_connection_body_mb": "单连接请求体缓冲上限",
        "max_sse_retained_mb": "单流 SSE 保留上限",
        "retain_raw_sse": "false = compact SSE 归约；true = 保留原始事件序列",
        "connection_idle_timeout_secs": "连接空闲超时",
        "connection_hard_timeout_secs": "连接硬超时，超时产出 incomplete",
        "ffi_channel_max_bytes_mb": "FFI 队列 + 回调在途字节预算",
    }
    rows: list[str] = []
    for key, note in notes.items():
        if key not in limits:
            continue
        value = limits[key]
        if key.endswith("_mb"):
            shown = f"{value:,} MiB"
        elif key.endswith("_secs"):
            shown = f"{value:,} s"
        elif key == "cpu_budget_percent":
            shown = f"{value} %"
        elif isinstance(value, bool):
            shown = "true" if value else "false"
        elif isinstance(value, int):
            shown = f"{value:,}"
        else:
            shown = str(value)
        rows.append(
            f"<tr><td><code>{html.escape(key)}</code></td><td>{html.escape(shown)}</td>"
            f"<td>{html.escape(note)}</td></tr>"
        )
    storage = config.get("storage") if isinstance(config.get("storage"), dict) else {}
    total = storage.get("max_total_size_mb")
    total_text = f"{total}" if total is not None else "N/A"
    return f"""
<details id="runtime-limits">
  <summary>运行限制（runtime_limits，本次 run 实际值）</summary>
  <p class="sub">来源 <code>config.json</code>；这些值共同决定连接缓存、入口队列、GenAI pending、SSE 和 ring 的内存上界。</p>
  <table><thead><tr><th>字段</th><th>本次值</th><th>作用</th></tr></thead><tbody>{''.join(rows)}</tbody></table>
  <p class="sub">存储：<code>storage.max_total_size_mb={html.escape(total_text)}</code>；各 store 的 retention / max_db_size / check_interval 使用代码默认值，除非 config.json 覆盖。</p>
  <p class="sub">其他资源相关默认值：<code>poll_timeout_ms=100</code>、<code>min_duration_us=10000</code>、<code>max_headers=64</code>；它们不属于 <code>runtime_limits</code>。</p>
</details>
"""


def resource_leak_section(summary: dict[str, Any]) -> str:
    resources = summary.get("resources") if isinstance(summary.get("resources"), dict) else {}
    keys = [
        ("threads", "线程（Threads）"),
        ("file_descriptors", "文件描述符（FD）"),
        ("active_connections", "Socket fd（active_connections）"),
    ]
    rows: list[str] = []
    cards: list[str] = []
    for key, label in keys:
        item = resources.get(key) if isinstance(resources.get(key), dict) else {}
        slope = item.get("slope_per_hour")
        slope_text = f"{slope:+.1f}/h" if isinstance(slope, (int, float)) else "—"
        first = item.get("first")
        last = item.get("last")
        maximum = item.get("max")
        verdict = "未增长（窗口过短）" if isinstance(slope, (int, float)) and slope <= 0 else "需关注"
        rows.append(
            f"<tr><td>{html.escape(label)}</td>"
            f"<td>{fmt_value(first, '', 0)}</td><td>{fmt_value(last, '', 0)}</td>"
            f"<td>{fmt_value(maximum, '', 0)}</td><td>{html.escape(slope_text)}</td>"
            f"<td>{html.escape(verdict)}</td></tr>"
        )
        cards.append(
            kpi(label, slope_text, f"首 {fmt_value(first, '', 0)} → 末 {fmt_value(last, '', 0)}")
        )
    return f"""
<section id="resource-leak"><h2>资源泄漏</h2>
<div class="kpis" style="margin-top: 0">{''.join(cards)}</div>
<table><thead><tr><th>资源</th><th>首值</th><th>末值</th><th>最大值</th><th>斜率</th><th>结论</th></tr></thead><tbody>{''.join(rows)}</tbody></table>
<p class="sub">60 秒样本的斜率只能看方向，不能证明无泄漏；4 小时长稳需要看 FD/线程/Socket 的首末差值和长窗口斜率。</p>
</section>
"""


def warmup_slope(
    times: Sequence[float | None],
    values: Sequence[float | None],
    warmup: float,
    window: float,
) -> tuple[float | None, list[float], list[float]]:
    pairs = [
        (time, value)
        for time, value in zip(times, values)
        if time is not None and value is not None and time >= warmup
    ]
    if len(pairs) < 2:
        return None, [], []
    post_slope = slope_per_hour([time for time, _ in pairs], [value for _, value in pairs])
    rolling_times: list[float] = []
    rolling_values: list[float] = []
    for time, _ in pairs:
        if time < warmup + window:
            continue
        subset = [(a, b) for a, b in pairs if time - window <= a <= time]
        if len(subset) < 3:
            continue
        value = slope_per_hour([a for a, _ in subset], [b for _, b in subset])
        if value is not None:
            rolling_times.append(time)
            rolling_values.append(value)
    return post_slope, rolling_times, rolling_values


def render(args: argparse.Namespace) -> Path:
    rows, times, columns = read_metrics(args.metrics)
    summary = load_json(args.run_summary)
    report = load_json(args.report_json)
    run_dir = args.metrics.parent
    worker = load_json(run_dir / "worker-result.json")

    defs = metric_defs(columns, args.rss_target_mb, args.connection_capacity)
    charts: list[tuple[str, str, str, str, dict]] = []
    chart_scripts: list[str] = []

    sections: dict[str, list[str]] = {}
    for item in defs:
        values = columns.get(item.key)
        if not values:
            continue
        scaled = [
            value / item.scale if value is not None and item.scale != 1.0 else value
            for value in values
        ]
        reference = (
            (item.reference, item.reference_label or "")
            if item.reference is not None and item.reference_label
            else None
        )
        chart_id = f"chart-{item.key}"
        chart_trend = item.trend and item.key != "rss_mb"
        svg, data = build_chart(
            chart_id,
            item.title,
            times,
            [
                PlotSeries(
                    item.title,
                    item.unit,
                    f"var({SERIES[0]})",
                    SERIES[0],
                    scaled,
                    item.digits,
                )
            ],
            reference,
            chart_trend,
            args.max_points,
        )
        chart_scripts.append(
            f'<script type="application/json" data-chart="{chart_id}">'
            f"{json_for_html(data)}</script>"
        )
        meta = f"峰值 {fmt_value(max(v for v in scaled if v is not None), item.unit, item.digits)}"
        if chart_trend:
            fit = slope_per_hour(times, scaled)
            if fit is not None:
                meta += f" · 趋势 {fit:+.1f} {item.unit}/h"
        card = (
            f'<article class="card"><h3>{html.escape(item.title)}</h3>'
            f'<div class="meta">{html.escape(meta)}</div>{svg}</article>'
        )
        sections.setdefault(item.section, []).append(card)

    # Cumulative loss counters: one chart, or an explicit empty state when clean.
    loss_keys = [
        ("eviction_count", "连接淘汰", SERIES[0]),
        ("ring_buffer_dropped", "Ring buffer 丢弃", SERIES[1]),
        ("channel_dropped", "Channel 丢弃", SERIES[2]),
        ("ffi_dropped", "FFI 丢弃", SERIES[0]),
    ]
    loss_series = [
        PlotSeries(label, "事件", f"var({color})", color, columns[key], 0)
        for key, label, color in loss_keys
        if key in columns and any(value not in (None, 0.0) for value in columns[key])
    ]
    loss_total = sum(counter_delta(plot.values) or 0.0 for plot in loss_series)
    if not loss_series or loss_total <= 0:
        sections.setdefault("丢失", []).append(
            '<article class="card"><h3>丢失计数</h3>'
            f'<div class="empty"><span class="status status-good">{status_icon("good")}'
            "<span>未记录连接淘汰、Ring buffer 丢弃、Channel 丢弃或 FFI 丢弃"
            "</span></span></div></article>"
        )
    else:
        chart_id = "chart-loss"
        single = len(loss_series) == 1
        title = (
            f"累计{loss_series[0].label}"
            if single
            else "累计丢失计数"
        )
        svg, data = build_chart(
            chart_id,
            title,
            times,
            loss_series,
            None,
            False,
            args.max_points,
        )
        chart_scripts.append(
            f'<script type="application/json" data-chart="{chart_id}">'
            f"{json_for_html(data)}</script>"
        )
        legend = ""
        if not single:
            legend = "".join(
                f'<span><span class="swatch" style="background: var({plot.color_name})"></span>'
                f"{html.escape(plot.label)}</span>"
                for plot in loss_series
            )
        legend_html = f'<div class="legend">{legend}</div>' if legend else ""
        sections.setdefault("丢失", []).append(
            f'<article class="card"><h3>{html.escape(title)}</h3>'
            f"{legend_html}{svg}</article>"
        )

    # Completion rate: derivative of the cumulative completed-events counter.
    completed = columns.get("completed")
    if completed is not None:
        rate = derive_rate(times, completed)
        chart_id = "chart-completed-rate"
        svg, data = build_chart(
            chart_id,
            "每秒完成事件数",
            times,
            [PlotSeries("事件", "事件/s", f"var({SERIES[0]})", SERIES[0], rate, 0)],
            None,
            False,
            args.max_points,
        )
        chart_scripts.append(
            f'<script type="application/json" data-chart="{chart_id}">'
            f"{json_for_html(data)}</script>"
        )
        sections.setdefault("进度", []).append(
            f'<article class="card"><h3>每秒完成事件数</h3>'
            f'<div class="meta">累计计数的 5 个采样滑动平均</div>{svg}</article>'
        )

    # Headline numbers.
    rss = stats(times, columns.get("rss_mb", []))
    cpu = stats(times, columns.get("cpu_pct", []))
    pending = stats(times, columns.get("pending_connection_count", []))
    evictions = counter_delta(columns.get("eviction_count"))
    drops = sum(
        counter_delta(columns.get(key)) or 0.0
        for key in ("ring_buffer_dropped", "channel_dropped", "ffi_dropped")
    )
    duration = summary.get("measured_seconds")
    if not isinstance(duration, (int, float)):
        duration = max((time for time in times if time is not None), default=0.0)

    completeness = report.get("completeness_ratio")
    if not isinstance(completeness, (int, float)):
        completeness = summary.get("trace_completeness")
    token_accuracy = report.get("token_accuracy")
    if not isinstance(token_accuracy, (int, float)):
        token_accuracy = summary.get("token_accuracy")
    process_survived = summary.get("process_survived")
    safety_stop = (args.metrics.parent / "safety-stop.json").is_file()
    if not safety_stop:
        safety_stop = (args.metrics.parent.parent / "safety-stop.json").is_file()
    if isinstance(process_survived, bool):
        survived = process_survived and not safety_stop
    else:
        alive = columns.get("process_alive")
        survived = (first_value(alive) == 1.0 if alive else True) and not safety_stop

    sent = report.get("sent")
    complete = report.get("complete")
    rejected = report.get("rejected")
    rejected_request_ids = report.get("rejected_request_ids")
    capacity_rejected_events = report.get("capacity_rejected_events")
    conflicts = report.get("conflicts")
    admitted_completeness = report.get("admitted_completeness")
    counts_available = all(
        isinstance(value, int) and not isinstance(value, bool)
        for value in (sent, complete, rejected)
    )
    if (
        not isinstance(admitted_completeness, (int, float))
        and counts_available
        and sent - rejected > 0
    ):
        admitted_completeness = complete / (sent - rejected)
    unexplained_loss = report.get("unexplained_loss")
    if (
        not isinstance(unexplained_loss, int)
        and counts_available
    ):
        unexplained_loss = sent - complete - rejected

    def ratio_text(value: Any) -> str:
        return f"{value * 100:.1f}%" if isinstance(value, (int, float)) else "—"

    def count_text(value: Any) -> str:
        return (
            str(value)
            if isinstance(value, int) and not isinstance(value, bool)
            else "—"
        )

    has_capacity_metrics = isinstance(capacity_rejected_events, int) and not isinstance(
        capacity_rejected_events, bool
    )
    rejected_formula = (
        "capacity_rejected（metrics）" if has_capacity_metrics else "rejected（request_id）"
    )
    rejected_description = (
        "metrics 总数；可逐请求归因 " + count_text(rejected_request_ids) + " 条"
        if has_capacity_metrics
        else "仅统计可识别 request_id 的主动拒绝"
    )

    completeness_section = (
        "<section><h2>采集完整性</h2><table>"
        "<thead><tr><th>指标</th><th>数值</th><th>公式</th><th>说明</th></tr></thead><tbody>"
        + "".join(
            f"<tr><td>{html.escape(label)}</td><td>{html.escape(value)}</td>"
            f"<td><code>{html.escape(formula)}</code></td>"
            f"<td>{html.escape(description)}</td></tr>"
            for label, value, formula, description in (
                (
                    "端到端完整率",
                    ratio_text(completeness),
                    "complete / sent",
                    "端到端采集率",
                ),
                (
                    "主动拒绝",
                    count_text(rejected),
                    rejected_formula,
                    rejected_description,
                ),
                (
                    "有效完整率",
                    ratio_text(admitted_completeness),
                    "complete / (sent - rejected)",
                    "排除主动背压后的真实采集完整性",
                ),
                (
                    "未解释丢失",
                    count_text(unexplained_loss),
                    "sent - complete - rejected",
                    "必须为 0",
                ),
                (
                    "冲突请求数",
                    count_text(conflicts),
                    "同时出现 complete 和 incomplete 的 request_id",
                    "必须为 0",
                ),
            )
        )
        + "</tbody></table>"
        + (
            "<p class=\"sub\">主动拒绝总数来自 metrics 的 capacity_rejected；其中 "
            + count_text(rejected_request_ids)
            + " 条可逐请求归因。</p>"
            if has_capacity_metrics
            else "<p class=\"sub\">rejected 仅统计采集记录中可识别 request_id 的主动拒绝。</p>"
        )
        + "</section>"
    )

    summary_effective_qps = summary.get("effective_qps")
    summary_input_qps = summary.get("input_qps")
    summary_tps = summary.get("tokens_per_second")
    summary_tps = summary_tps if isinstance(summary_tps, dict) else {}
    summary_latency = summary.get("latency_ms")
    summary_latency = summary_latency if isinstance(summary_latency, dict) else {}
    summary_drop_rate = summary.get("drop_rate")
    summary_capture_loss_rate = summary.get("capture_loss_rate")
    summary_drop_source = summary.get("drop_rate_source") or "N/A"
    drop_rate_text = (
        f"{summary_drop_rate * 100:.2f}%"
        if isinstance(summary_drop_rate, (int, float))
        else "—"
    )
    capture_loss_text = (
        f"{summary_capture_loss_rate * 100:.2f}%"
        if isinstance(summary_capture_loss_rate, (int, float))
        else "—"
    )
    rss_values = columns.get("rss_mb", [])
    post_slope, rolling_times, rolling_values = warmup_slope(
        times, rss_values, args.warmup_seconds, args.slope_window_seconds
    )
    rss_at_warmup = next(
        (
            value
            for time, value in zip(times, rss_values)
            if time is not None and value is not None and time >= args.warmup_seconds
        ),
        None,
    )
    target_mb = args.rss_target_mb
    rss_svg, rss_data = build_chart(
        "chart-rss-warmup",
        "常驻内存（warmup 后）",
        times,
        [
            PlotSeries(
                "常驻内存",
                "MiB",
                f"var({SERIES[0]})",
                SERIES[0],
                rss_values,
                1,
            )
        ],
        (target_mb, f"目标 {target_mb:g} MiB"),
        True,
        args.max_points,
        warmup=args.warmup_seconds,
        trend_from=args.warmup_seconds,
    )
    chart_scripts.append(
        f'<script type="application/json" data-chart="chart-rss-warmup">'
        f"{json_for_html(rss_data)}</script>"
    )
    rolling_card = (
        '<article class="card"><h3>滚动 RSS 斜率</h3>'
        '<div class="empty">样本不足，无法计算滚动斜率</div></article>'
    )
    if rolling_times:
        rolling_svg, rolling_data = build_chart(
            "chart-rss-rolling",
            "滚动 RSS 斜率",
            rolling_times,
            [
                PlotSeries(
                    f"{args.slope_window_seconds:g}s 滚动斜率",
                    "MiB/h",
                    f"var({SERIES[1]})",
                    SERIES[1],
                    rolling_values,
                    0,
                )
            ],
            (
                (post_slope, f"warmup 后 +{post_slope:.0f} MiB/h")
                if isinstance(post_slope, (int, float))
                else None
            ),
            False,
            args.max_points,
        )
        chart_scripts.append(
            f'<script type="application/json" data-chart="chart-rss-rolling">'
            f"{json_for_html(rolling_data)}</script>"
        )
        rolling_card = (
            '<article class="card"><h3>滚动 RSS 斜率</h3>'
            f'<div class="meta">{args.slope_window_seconds:g}s 窗口 · warmup 后 '
            f'+{post_slope:.1f} MiB/h</div>{rolling_svg}</article>'
        )
    rss_slope_section = (
        '<section><h2>RSS 斜率分析</h2><div class="grid">'
        '<article class="card"><h3>常驻内存（warmup 后）</h3>'
        f'<div class="meta">峰值 {fmt_value(rss["max"], "MiB", 1)} · '
        f'warmup {fmt_value(rss_at_warmup, "MiB", 1)} → '
        f'{fmt_value(rss["last"], "MiB", 1)} · 斜率 '
        f'{(f"+{post_slope:.1f} MiB/h") if isinstance(post_slope, (int, float)) else "—"}'
        f'</div>{rss_svg}</article>{rolling_card}</div>'
        f'<p class="sub">{args.warmup_seconds:g}s warmup、{args.slope_window_seconds:g}s 滚动窗口；'
        "窗口过短时斜率只能看方向，正式长稳建议 warmup 300–600s、滚动窗口 30–60min。</p>"
        "</section>"
    )

    throughput_section = (
        "<section><h2>吞吐与延迟</h2><table>"
        "<thead><tr><th>指标</th><th>数值</th><th>说明</th></tr></thead><tbody>"
        f"<tr><td>目标 QPS（input_qps）</td><td>{fmt_value(summary_input_qps, '', 1)}</td><td>配置目标</td></tr>"
        f"<tr><td>实际 QPS（effective_qps）</td><td>{fmt_value(summary_effective_qps, '', 1)}</td><td>sent_requests / measured_seconds</td></tr>"
        f"<tr><td>生成 TPS</td><td>{fmt_value(summary_tps.get('generated'), 'tok/s', 1)}</td><td>负载生成器</td></tr>"
        f"<tr><td>采集 TPS</td><td>{fmt_value(summary_tps.get('captured'), 'tok/s', 1)}</td><td>AgentSight 采集</td></tr>"
        f"<tr><td>Latency P50</td><td>{fmt_value((summary_latency.get('p50') or 0) / 1000 if summary_latency.get('p50') is not None else None, 's', 2)}</td><td>典型请求</td></tr>"
        f"<tr><td>Latency P95</td><td>{fmt_value((summary_latency.get('p95') or 0) / 1000 if summary_latency.get('p95') is not None else None, 's', 2)}</td><td>尾部 5%</td></tr>"
        f"<tr><td>Latency P99</td><td>{fmt_value((summary_latency.get('p99') or 0) / 1000 if summary_latency.get('p99') is not None else None, 's', 2)}</td><td>尾部 1%</td></tr>"
        f"<tr><td>Latency max</td><td>{fmt_value((summary_latency.get('max') or 0) / 1000 if summary_latency.get('max') is not None else None, 's', 2)}</td><td>最慢请求</td></tr>"
        "</tbody></table>"
        '<p class="sub">当前 mock 的延迟基线由 tokens-per-response / tokens-per-second 决定；P99 高于基线才说明额外排队。</p>'
        "</section>"
    )

    kpis = [
        kpi("有效 QPS", fmt_value(summary_effective_qps, "", 1), f"目标 {fmt_value(summary_input_qps, '', 1)}"),
        kpi("Token 吞吐", fmt_value(summary_tps.get("generated"), "", 1), f"tok/s · 采集 {fmt_value(summary_tps.get('captured'), '', 1)}"),
        kpi(
            "Latency P99",
            fmt_value(
                (summary_latency.get("p99") or 0) / 1000
                if summary_latency.get("p99") is not None
                else None,
                "s",
                2,
            ),
            f"P50 {fmt_value((summary_latency.get('p50') or 0) / 1000 if summary_latency.get('p50') is not None else None, 's', 2)}",
        ),
        kpi(
            f"RSS 斜率（{args.warmup_seconds:g}s 后）",
            f"{post_slope:+.1f} MiB/h" if isinstance(post_slope, (int, float)) else "—",
            f"{args.warmup_seconds:g}s warmup 后拟合",
        ),
        kpi("统计窗口", fmt_time(float(duration)), f"{len(rows)} 个采样点"),
        kpi(
            "RSS 峰值",
            fmt_value(rss["max"], "MiB", 1),
            f"{fmt_value(rss['first'], 'MiB', 1)} → {fmt_value(rss['last'], 'MiB', 1)}",
        ),
        kpi("RSS 平均", fmt_value(rss["avg"], "MiB", 1), f"P50 {fmt_value(rss['p50'], 'MiB', 1)}"),
        kpi("CPU 峰值", fmt_value(cpu["max"], "%", 1), f"P95 {fmt_value(cpu['p95'], '%', 1)}"),
        kpi("CPU 平均", fmt_value(cpu["avg"], "%", 1), f"P50 {fmt_value(cpu['p50'], '%', 1)}"),
        kpi(
            "在途连接峰值",
            fmt_value(pending["max"], "", 0),
            "连接或流",
        ),
        kpi(
            "淘汰次数",
            fmt_value(evictions, "", 0),
            "运行期间累计",
            "warning" if (evictions or 0) > 0 else "good",
            "有" if (evictions or 0) > 0 else "无",
        ),
        kpi(
            "丢弃次数",
            fmt_value(drops, "", 0),
            "Ring buffer + Channel + FFI",
            "critical" if drops > 0 else "good",
            "有" if drops > 0 else "无",
        ),
        kpi("事件丢弃率", drop_rate_text, summary_drop_source),
        kpi("请求丢失率", capture_loss_text, "1 - trace completeness"),
        kpi(
            "进程状态",
            "存活" if survived else "未存活",
            "未触发安全停止" if not safety_stop else "触发了安全停止",
            "good" if survived else "critical",
            "正常" if survived else "检查日志",
        ),
        kpi(
            "有效完整率",
            ratio_text(admitted_completeness),
            "complete / (sent - rejected)",
            (
                "good"
                if isinstance(admitted_completeness, (int, float))
                and admitted_completeness >= 0.999
                else "warning"
            ),
            "完整" if isinstance(admitted_completeness, (int, float)) else "不可用",
        ),
        kpi(
            "Token 准确率",
            ratio_text(token_accuracy),
            "已校验 token 总数",
            (
                "good"
                if isinstance(token_accuracy, (int, float)) and token_accuracy >= 0.999
                else "warning"
            ),
            "已验证" if isinstance(token_accuracy, (int, float)) else "不可用",
        ),
    ]

    stability_html = stability_section(run_dir, worker)
    fault_html = fault_section(run_dir)
    runtime_html = runtime_limits_section(run_dir)
    resource_html = resource_leak_section(summary)

    # Summary table, which also provides the required table view.
    table_rows: list[str] = []
    for item in defs:
        values = columns.get(item.key)
        if not values:
            continue
        scaled = [
            value / item.scale if value is not None and item.scale != 1.0 else value
            for value in values
        ]
        summary_stats = stats(times, scaled)
        table_rows.append(
            "<tr>"
            f"<td>{html.escape(item.title)}</td>"
            f"<td>{html.escape(fmt_value(summary_stats['first'], item.unit, item.digits))}</td>"
            f"<td>{html.escape(fmt_value(summary_stats['last'], item.unit, item.digits))}</td>"
            f"<td>{html.escape(fmt_value(summary_stats['min'], item.unit, item.digits))}</td>"
            f"<td>{html.escape(fmt_value(summary_stats['max'], item.unit, item.digits))}</td>"
            f"<td>{html.escape(fmt_value(summary_stats['avg'], item.unit, item.digits))}</td>"
            f"<td>{html.escape(fmt_value(summary_stats['p95'], item.unit, item.digits))}</td>"
            f"<td>{html.escape('—' if item.key == 'rss_mb' else fmt_value(summary_stats['slope'], item.unit + '/h', item.digits))}</td>"
            "</tr>"
        )
    for plot in loss_series:
        summary_stats = stats(times, plot.values)
        table_rows.append(
            "<tr>"
            f"<td>{html.escape(plot.label)}</td>"
            f"<td>{html.escape(fmt_value(summary_stats['first'], plot.unit, 0))}</td>"
            f"<td>{html.escape(fmt_value(summary_stats['last'], plot.unit, 0))}</td>"
            f"<td>{html.escape(fmt_value(summary_stats['min'], plot.unit, 0))}</td>"
            f"<td>{html.escape(fmt_value(summary_stats['max'], plot.unit, 0))}</td>"
            f"<td>{html.escape(fmt_value(summary_stats['avg'], plot.unit, 0))}</td>"
            f"<td>{html.escape(fmt_value(summary_stats['p95'], plot.unit, 0))}</td>"
            f"<td>{html.escape(fmt_value(summary_stats['slope'], plot.unit + '/h', 0))}</td>"
            "</tr>"
        )

    section_html: list[str] = []
    for section, cards in sections.items():
        section_html.append(
            f'<section><h2>{html.escape(section)}</h2><div class="grid">{"".join(cards)}</div>'
            "</section>"
        )

    source_name = args.metrics.name
    document = f"""<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{html.escape(args.title)}</title>
<style>
:root {{
  color-scheme: light;
  --page: #f9f9f7;
  --surface-1: #fcfcfb;
  --text-primary: #0b0b0b;
  --text-secondary: #52514e;
  --text-muted: #898781;
  --grid: #e1e0d9;
  --axis: #c3c2b7;
  --border: rgba(11, 11, 11, 0.10);
  --series-1: #2a78d6;
  --series-2: #eb6834;
  --series-3: #1baf7a;
  --status-good: #0ca30c;
  --status-warning: #fab219;
  --status-critical: #d03b3b;
}}
@media (prefers-color-scheme: dark) {{
  :root:where(:not([data-theme="light"])) {{
    color-scheme: dark;
    --page: #0d0d0d;
    --surface-1: #1a1a19;
    --text-primary: #f0efec;
    --text-secondary: #c3c2b7;
    --text-muted: #898781;
    --grid: #2c2c2a;
    --axis: #383835;
    --border: rgba(255, 255, 255, 0.10);
    --series-1: #3987e5;
    --series-2: #d95926;
    --series-3: #199e70;
  }}
}}
:root[data-theme="dark"] {{
  color-scheme: dark;
  --page: #0d0d0d;
  --surface-1: #1a1a19;
  --text-primary: #f0efec;
  --text-secondary: #c3c2b7;
  --text-muted: #898781;
  --grid: #2c2c2a;
  --axis: #383835;
  --border: rgba(255, 255, 255, 0.10);
  --series-1: #3987e5;
  --series-2: #d95926;
  --series-3: #199e70;
}}
* {{ box-sizing: border-box; }}
body {{
  margin: 0;
  background: var(--page);
  color: var(--text-primary);
  font-family: system-ui, -apple-system, "Segoe UI", sans-serif;
}}
.viz-root {{ max-width: 1480px; margin: 0 auto; padding: 32px 24px 64px; }}
header {{ display: flex; align-items: flex-start; justify-content: space-between; gap: 24px; }}
h1 {{ font-size: 22px; margin: 0 0 6px; letter-spacing: -0.01em; }}
.sub {{ margin: 0; color: var(--text-secondary); font-size: 13px; }}
.sub code {{ font-size: 12px; }}
button {{
  border: 1px solid var(--border); background: var(--surface-1); color: var(--text-primary);
  border-radius: 8px; padding: 7px 12px; font: inherit; font-size: 12px; cursor: pointer;
}}
.kpis {{
  display: grid; grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
  gap: 12px; margin-top: 24px;
}}
.kpi {{
  background: var(--surface-1); border: 1px solid var(--border);
  border-radius: 12px; padding: 13px 15px; min-height: 92px;
}}
.kpi .label {{
  font-size: 11px; color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.05em;
}}
.kpi .value {{
  font-size: 23px; font-weight: 600; margin-top: 5px; font-variant-numeric: tabular-nums;
}}
.kpi .sub {{ font-size: 11px; color: var(--text-secondary); margin-top: 5px; display: flex;
  flex-wrap: wrap; align-items: center; gap: 5px; overflow-wrap: anywhere; }}
h2 {{ font-size: 15px; margin: 30px 0 12px; }}
.grid {{
  display: grid; grid-template-columns: repeat(auto-fit, minmax(430px, 1fr)); gap: 16px;
}}
.card {{
  background: var(--surface-1); border: 1px solid var(--border);
  border-radius: 12px; padding: 14px 16px 8px; min-width: 0;
}}
.card h3 {{ font-size: 14px; margin: 0 0 2px; }}
.card .meta {{ font-size: 11px; color: var(--text-muted); margin-bottom: 4px; }}
.legend {{ display: flex; flex-wrap: wrap; gap: 14px; font-size: 11px;
  color: var(--text-secondary); margin: 4px 0 2px; }}
.legend > span {{ display: inline-flex; align-items: center; gap: 6px; }}
.swatch {{ width: 10px; height: 10px; border-radius: 3px; display: inline-block; }}
svg.chart {{ width: 100%; height: auto; display: block; overflow: visible; }}
.axis {{ fill: var(--text-muted); font-size: 11px; font-variant-numeric: tabular-nums; }}
.gridline {{ stroke: var(--grid); stroke-width: 1; }}
.baseline {{ stroke: var(--axis); stroke-width: 1; }}
.series-line {{ fill: none; stroke-width: 2; stroke-linejoin: round; stroke-linecap: round; }}
.reference {{ stroke: var(--text-muted); stroke-width: 1.5; stroke-dasharray: 5 4; }}
.trend {{ stroke: var(--text-muted); stroke-width: 1.5; stroke-dasharray: 2 4; }}
.last-label {{ fill: var(--text-secondary); font-size: 11px; font-weight: 500;
  font-variant-numeric: tabular-nums; paint-order: stroke;
  stroke: var(--surface-1); stroke-width: 3px; stroke-linejoin: round; }}
.crosshair {{ stroke: var(--text-muted); stroke-width: 1; stroke-dasharray: 3 3; }}
.hit {{ fill: transparent; cursor: crosshair; }}
.empty {{ display: flex; align-items: center; padding: 26px 4px 30px;
  color: var(--text-secondary); font-size: 13px; }}
.status {{ display: inline-flex; align-items: center; gap: 5px; }}
.status svg {{ width: 14px; height: 14px; flex: none; }}
.status-good {{ color: var(--status-good); }}
.status-warning {{ color: var(--status-warning); }}
.status-critical {{ color: var(--status-critical); }}
details {{ margin-top: 32px; background: var(--surface-1); border: 1px solid var(--border);
  border-radius: 12px; padding: 14px 16px; }}
summary {{ cursor: pointer; font-size: 13px; font-weight: 600; }}
table {{ width: 100%; border-collapse: collapse; margin-top: 12px; font-size: 12px;
  font-variant-numeric: tabular-nums; }}
th, td {{ padding: 7px 8px; text-align: right; border-bottom: 1px solid var(--grid); }}
th:first-child, td:first-child {{ text-align: left; }}
#stability td:nth-child(3), #stability th:nth-child(3),
#stability td:nth-child(4), #stability th:nth-child(4),
#fault-injection td:nth-child(1), #fault-injection th:nth-child(1),
#fault-injection td:nth-child(3), #fault-injection th:nth-child(3),
#fault-injection td:nth-child(4), #fault-injection th:nth-child(4),
#resource-leak td:last-child, #resource-leak th:last-child,
#runtime-limits td:nth-child(3), #runtime-limits th:nth-child(3) {{ text-align: left; }}
th {{ color: var(--text-muted); font-weight: 500; font-size: 11px; }}
footer {{ margin-top: 24px; color: var(--text-muted); font-size: 11px; }}
#tooltip {{
  position: fixed; z-index: 20; pointer-events: none; min-width: 180px; max-width: 320px;
  background: var(--surface-1); border: 1px solid var(--border); border-radius: 10px;
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.18); padding: 9px 11px; font-size: 12px;
}}
#tooltip .tip-time {{ color: var(--text-muted); font-size: 11px; margin-bottom: 5px; }}
#tooltip .tip-row {{ display: flex; align-items: center; gap: 7px; margin-top: 3px; }}
#tooltip .tip-row b {{ margin-left: auto; font-variant-numeric: tabular-nums; }}
@media (max-width: 640px) {{
  .viz-root {{ padding: 20px 14px 48px; }}
  .grid {{ grid-template-columns: 1fr; }}
}}
@media print {{
  :root {{ color-scheme: light; --page: #fff; --surface-1: #fff; }}
  button {{ display: none; }}
}}
</style>
</head>
<body>
<div class="viz-root">
<header>
  <div>
    <h1>{html.escape(args.title)}</h1>
    <p class="sub">数据源 <code>{html.escape(source_name)}</code> · {len(rows)} 个采样点 · 尽力评估报告</p>
  </div>
  <button id="theme-toggle" type="button" aria-label="切换主题">主题</button>
</header>

<section class="kpis" aria-label="运行摘要">
  {''.join(kpis)}
</section>

{completeness_section}

{throughput_section}

{stability_html}

{fault_html}

{''.join(section_html)}

{resource_html}

{rss_slope_section}

{runtime_html}

<details>
  <summary>汇总表</summary>
  <table>
    <thead><tr><th>指标</th><th>首值</th><th>末值</th><th>最小值</th><th>最大值</th>
    <th>平均值</th><th>P95</th><th>斜率/h</th></tr></thead>
    <tbody>{''.join(table_rows)}</tbody>
  </table>
</details>

<footer>只读评估视图：<code>{html.escape(source_name)}</code>。单位已按可读性缩放。</footer>
</div>
<div id="tooltip" role="status" hidden></div>
{''.join(chart_scripts)}
<script>
(function () {{
  const data = {{}};
  document.querySelectorAll('script[data-chart]').forEach(function (node) {{
    data[node.dataset.chart] = JSON.parse(node.textContent);
  }});

  const tooltip = document.getElementById('tooltip');

  function fmtTime(seconds) {{
    if (!Number.isFinite(seconds)) return '—';
    seconds = Math.max(0, seconds);
    if (seconds < 60) return seconds.toFixed(0) + 's';
    const minutes = Math.floor(seconds / 60);
    const rest = Math.round(seconds % 60);
    if (minutes < 60) return minutes + 'm' + String(rest).padStart(2, '0') + 's';
    const hours = Math.floor(minutes / 60);
    return hours + 'h' + String(minutes % 60).padStart(2, '0') + 'm';
  }}

  function fmtValue(value, unit, digits) {{
    if (value === null || value === undefined || !Number.isFinite(value)) return '—';
    const text = Number(value).toFixed(digits);
    return unit ? text + ' ' + unit : text;
  }}

  document.querySelectorAll('svg.chart').forEach(function (svg) {{
    const chart = data[svg.dataset.chart];
    const hit = svg.querySelector('.hit');
    const crosshair = svg.querySelector('.crosshair');
    const dots = Array.from(svg.querySelectorAll('.hover-dot'));
    if (!chart || !hit || !crosshair || !dots.length) return;

    function hide() {{
      crosshair.style.display = 'none';
      dots.forEach(function (dot) {{ dot.style.display = 'none'; }});
      tooltip.hidden = true;
    }}

    function move(event) {{
      const point = svg.createSVGPoint();
      point.x = event.clientX;
      point.y = event.clientY;
      const local = point.matrixTransform(svg.getScreenCTM().inverse());
      const plot = chart.plot;
      if (local.x < plot.left || local.x > plot.left + plot.width) {{ hide(); return; }}
      const times = chart.t;
      const first = times.find(function (value) {{ return value !== null; }});
      const last = times.slice().reverse().find(function (value) {{ return value !== null; }});
      if (first === undefined || last === undefined) return;
      const target = first + (local.x - plot.left) / plot.width * (last - first);
      let index = 0;
      let best = Infinity;
      for (let i = 0; i < times.length; i += 1) {{
        if (times[i] === null) continue;
        const distance = Math.abs(times[i] - target);
        if (distance < best) {{ best = distance; index = i; }}
      }}
      const time = times[index];
      if (time === null) {{ hide(); return; }}
      const x = plot.left + (time - first) / (last - first || 1) * plot.width;
      crosshair.setAttribute('x1', x.toFixed(1));
      crosshair.setAttribute('x2', x.toFixed(1));
      crosshair.setAttribute('y1', plot.top);
      crosshair.setAttribute('y2', plot.top + plot.height);
      crosshair.style.display = '';
      let rows = '';
      chart.series.forEach(function (series, seriesIndex) {{
        const value = series.v[index];
        const dot = dots[seriesIndex];
        if (value === null || value === undefined || !Number.isFinite(value)) {{
          dot.style.display = 'none';
          return;
        }}
        const y = plot.top + (1 - (value - chart.yMin) / (chart.yMax - chart.yMin)) * plot.height;
        dot.setAttribute('cx', x.toFixed(1));
        dot.setAttribute('cy', y.toFixed(1));
        dot.style.display = '';
        rows += '<div class="tip-row"><span class="swatch" style="background: var(' +
          series.color + ')"></span><span>' + series.label + '</span><b>' +
          fmtValue(value, series.unit, series.digits) + '</b></div>';
      }});
      tooltip.innerHTML = '<div class="tip-time">t = ' + fmtTime(time) + '</div>' + rows;
      tooltip.hidden = false;
      const margin = 14;
      const width = tooltip.offsetWidth || 200;
      const height = tooltip.offsetHeight || 80;
      const left = Math.min(window.innerWidth - width - margin, event.clientX + margin);
      const top = Math.min(window.innerHeight - height - margin, event.clientY + margin);
      tooltip.style.left = Math.max(margin, left) + 'px';
      tooltip.style.top = Math.max(margin, top) + 'px';
    }}

    hit.addEventListener('mousemove', move);
    hit.addEventListener('mouseleave', hide);
  }});

  const toggle = document.getElementById('theme-toggle');
  if (toggle) {{
    toggle.addEventListener('click', function () {{
      const root = document.documentElement;
      const dark = root.dataset.theme
        ? root.dataset.theme === 'dark'
        : window.matchMedia('(prefers-color-scheme: dark)').matches;
      root.dataset.theme = dark ? 'light' : 'dark';
    }});
  }}
}})();
</script>
</body>
</html>
"""

    try:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(document, encoding="utf-8")
    except OSError as error:
        raise SystemExit(f"cannot write {args.output}: {error}") from error
    return args.output


def main() -> int:
    args = parse_args()
    output = render(args)
    print(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
