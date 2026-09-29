# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Test that trace timestamps are monotonically ordered."""

import json
import pytest
from pathlib import Path

from .helpers import discover_task_ids, find_latest_trace, TRACES_DIR


def extract_timestamps(trace_path: Path) -> list[tuple[str, str]]:
    """Extract (event_type, timestamp) pairs from a trace JSONL file."""
    results: list[tuple[str, str]] = []
    with open(trace_path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue

            etype = event.get("type", "?")
            ts = event.get("timestamp")
            if ts is None:
                # Some events store timestamp inside message
                ts = event.get("message", {}).get("timestamp")

            if ts:
                results.append((etype, ts))

    return results


def are_timestamps_ordered(timestamps: list[str]) -> tuple[bool, list[str]]:
    """Check if timestamps are monotonically increasing.

    Returns (ordered, list_of_violations)
    """
    ordered = True
    violations: list[str] = []
    for i in range(1, len(timestamps)):
        if timestamps[i] < timestamps[i - 1]:
            ordered = False
            violations.append(
                f"#[{i-1}]({timestamps[i-1]}) > #[{i}]({timestamps[i]})"
            )
    return ordered, violations


# Discover task IDs at module level for parametrize
_DISCOVERED_TASK_IDS = discover_task_ids(TRACES_DIR)
# Fallback to known task if no traces found
if not _DISCOVERED_TASK_IDS:
    _DISCOVERED_TASK_IDS = ["T009zh_contact_lookup"]


def find_trace_files(base_dir: Path, keyword: str) -> tuple[Path | None, Path | None, str, str]:
    """Find the latest trace files for both native claw-eval and ce-runner.

    Returns (native_trace_path, ce_runner_trace_path, native_dir_name, ce_runner_dir_name)
    """
    native, native_dir = find_latest_trace(base_dir, keyword, dir_pattern="")
    ce_runner, ce_runner_dir = find_latest_trace(base_dir, keyword, dir_pattern="openclaw")
    return native, ce_runner, native_dir, ce_runner_dir


class TestTraceTimestampOrder:
    """Test that trace files have monotonically ordered timestamps."""

    @pytest.fixture(autouse=True)
    def setup(self):
        """Check if traces directory exists."""
        assert TRACES_DIR.exists(), f"Traces directory not found: {TRACES_DIR}"

    def _print_trace_info(self, trace_path: Path | None, dir_name: str, label: str):
        """Print which trace was selected."""
        if trace_path:
            print(f"\n  [{label}] 目录: traces/{dir_name}/")
            print(f"  [{label}] 文件: {trace_path.name}")
            print(f"  [{label}] 路径: {trace_path}")
        else:
            print(f"\n  [{label}] 未找到匹配文件")

    def _test_trace_ordered(self, trace_path: Path):
        """Helper: assert that a single trace file has ordered timestamps.

        Always prints trace comparison info regardless of pass/fail.
        """
        assert trace_path.exists(), f"Trace file not found: {trace_path}"

        entries = extract_timestamps(trace_path)
        assert len(entries) > 0, f"No timestamped events in {trace_path.name}"

        ts_list = [ts for _, ts in entries]
        ordered, violations = are_timestamps_ordered(ts_list)

        # Always print trace comparison info
        print(f"\n  [{trace_path.name}]")
        print(f"    事件数: {len(entries)}")
        print(f"    起始:   {ts_list[0]}")
        print(f"    结束:   {ts_list[-1]}")
        print(f"    顺序:   {'PASS' if ordered else 'FAIL'}")

        if not ordered:
            detail = "\n".join(
                f"    {etype:20s} {ts}" for etype, ts in entries
            )
            pytest.fail(
                f"Trace {trace_path.name} has {len(violations)} "
                f"out-of-order timestamp(s):\n"
                + "\n".join(f"    ✗ {v}" for v in violations)
                + f"\n\n  Full sequence:\n{detail}"
            )

    @pytest.mark.parametrize("keyword", _DISCOVERED_TASK_IDS)
    def test_ce_runner_trace_timestamps_ordered(self, keyword: str):
        """ce-runner generated trace should have ordered timestamps."""
        _, ce_runner_trace, _, ce_runner_dir = find_trace_files(TRACES_DIR, keyword)
        self._print_trace_info(ce_runner_trace, ce_runner_dir, "ce-runner")
        if ce_runner_trace is None:
            pytest.skip(f"No ce-runner trace found for '{keyword}'")
        self._test_trace_ordered(ce_runner_trace)

    @pytest.mark.parametrize("keyword", _DISCOVERED_TASK_IDS)
    def test_native_claw_eval_trace_timestamps_ordered(self, keyword: str):
        """Native claw-eval trace should have ordered timestamps."""
        native_trace, _, native_dir, _ = find_trace_files(TRACES_DIR, keyword)
        self._print_trace_info(native_trace, native_dir, "native")
        if native_trace is None:
            pytest.skip(f"No native claw-eval trace found for '{keyword}'")
        self._test_trace_ordered(native_trace)

    @pytest.mark.parametrize("keyword", _DISCOVERED_TASK_IDS)
    def test_both_traces_ordered(self, keyword: str):
        """Compare: both traces for a task should have ordered timestamps."""
        native, ce_runner, native_dir, ce_runner_dir = find_trace_files(TRACES_DIR, keyword)

        self._print_trace_info(native, native_dir, "native")
        self._print_trace_info(ce_runner, ce_runner_dir, "ce-runner")

        # Collect comparison data and always print a summary table
        results: dict[str, dict] = {}

        for label, trace_path in [("native", native), ("ce-runner", ce_runner)]:
            if trace_path:
                entries = extract_timestamps(trace_path)
                ts_list = [ts for _, ts in entries]
                ordered, violations = are_timestamps_ordered(ts_list)
                results[label] = {
                    "path": trace_path,
                    "events": len(entries),
                    "start": ts_list[0] if ts_list else "N/A",
                    "end": ts_list[-1] if ts_list else "N/A",
                    "ordered": ordered,
                    "violations": len(violations),
                }
            else:
                results[label] = None

        # Always print comparison summary
        print(f"\n  === 时间戳比较: {keyword} ===")
        print(f"  {'':12s} {'事件数':>8s} {'起始时间':>32s} {'结束时间':>32s} {'顺序':>6s}")
        print(f"  {'-'*12} {'-'*8} {'-'*32} {'-'*32} {'-'*6}")
        for label, r in results.items():
            if r:
                status = "PASS" if r["ordered"] else f"FAIL({r['violations']})"
                print(f"  {label:12s} {r['events']:>8d} {r['start']:>32s} {r['end']:>32s} {status:>6s}")
            else:
                print(f"  {label:12s} {'(未找到)':>8s}")
        print()

        # Actual assertions
        if native:
            self._test_trace_ordered(native)

        if ce_runner:
            self._test_trace_ordered(ce_runner)

        if native is None and ce_runner is None:
            pytest.skip(f"No traces found for '{keyword}'")
