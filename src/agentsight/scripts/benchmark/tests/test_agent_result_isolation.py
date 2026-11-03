"""A dead or damaged worker must leave a failed report with healthy evidence."""

from __future__ import annotations

import argparse
import gzip
import importlib.util
import json
from pathlib import Path
from typing import Any

import pytest
from test_resource_report import artifacts, resource_report

SCRIPT = Path(__file__).parents[1] / "single_run/load/agents.py"
spec = importlib.util.spec_from_file_location("isolated_agents", SCRIPT)
agents = importlib.util.module_from_spec(spec)
spec.loader.exec_module(agents)


def fixture(root: Path) -> argparse.Namespace:
    output = root / "agents"
    output.mkdir(parents=True, exist_ok=True)
    for index in range(2):
        (output / f"agent-{index}.json").write_text(
            json.dumps(
                {
                    "agent": index,
                    "pid": 100 + index,
                    "tokens_per_second": 100,
                }
            )
        )
        (output / f"agent-{index}.jsonl").write_text(
            json.dumps(
                {
                    "metric": "benchmark_requests",
                    "stream_start": 10,
                    "stream_end": 12,
                    "request_id": f"run-{index}",
                    "data": {"value": 1},
                }
            )
            + "\n"
        )
    return argparse.Namespace(
        output_dir=output,
        agents=2,
        duration=2,
        tokens_per_second=100,
        tokens_per_response=200,
        rate_tolerance=0.05,
    )


@pytest.mark.parametrize(
    "damage",
    [
        "missing",
        "truncated",
        "nonobject",
        "identity",
        "pid",
        "rate",
        "nan",
        "negative",
        "log",
        "log-shape",
        "interval",
        "missing-log",
    ],
)
def test_damaged_worker_isolated_and_healthy_worker_preserved(tmp_path: Path, damage: str) -> None:
    args = fixture(tmp_path)
    path = args.output_dir / "agent-0.json"
    log = path.with_suffix(".jsonl")
    record = json.loads(path.read_text())
    if damage == "missing":
        path.unlink()
    elif damage == "truncated":
        path.write_text('{"agent":')
    elif damage == "nonobject":
        path.write_text("null")
    elif damage in ("identity", "pid", "rate", "nan", "negative"):
        key, value = {
            "identity": ("agent", 1),
            "pid": ("pid", True),
            "rate": ("tokens_per_second", []),
            "nan": ("tokens_per_second", float("nan")),
            "negative": ("tokens_per_second", -1),
        }[damage]
        record[key] = value
        path.write_text(json.dumps(record))
    elif damage == "missing-log":
        log.unlink()
    else:
        line = {
            "log": "{torn",
            "log-shape": "[]",
            "interval": json.dumps(
                {"metric": "benchmark_requests", "stream_start": "bad", "stream_end": 12}
            ),
        }[damage]
        log.write_text(line + "\n")
    result = agents.summarize(args, {"start": 10, "end": 12})
    assert result["passed"] is False
    assert result["failed_agents"] == [0]
    assert result["agents"][1]["tokens_per_second"] == 100
    assert "error" not in result["agents"][1]
    with gzip.open(tmp_path / "agents.jsonl.gz", "rt") as stream:
        assert '"request_id": "run-1"' in stream.read()


def test_duplicate_worker_identity_cannot_prove_independent_clients(tmp_path: Path) -> None:
    args = fixture(tmp_path)
    path = args.output_dir / "agent-1.json"
    record = json.loads(path.read_text())
    record["pid"] = 100
    path.write_text(json.dumps(record))
    result = agents.summarize(args, {"start": 10, "end": 12})
    assert result["distinct_agent_pids"] == 1
    assert result["failed_agents"] == [0, 1]
    assert result["passed"] is False


def test_valid_workers_still_pass_and_count_distinct_pids(tmp_path: Path) -> None:
    args = fixture(tmp_path)
    result = agents.summarize(args, {"start": 10, "end": 12})
    assert result["passed"] is True
    assert result["distinct_agent_pids"] == result["peak_active_streams"] == 2
    assert result["average_active_streams"] == 2
    assert result["received_output_tokens_per_second"] == 200


def test_partial_worker_report_disables_ab_claims_without_losing_resource_stats(
    tmp_path: Path,
) -> None:
    before = artifacts(tmp_path, "before", 30, 400)
    after = artifacts(tmp_path, "after", 20, 200)
    args = fixture(after)
    (args.output_dir / "agent-0.json").unlink()
    result = agents.summarize(args, {"start": 10, "end": 12})
    (after / "agent-summary.json").write_text(json.dumps(result))
    assert resource_report.report(tmp_path, {"before": {}, "after": {}}) is False
    saved = json.loads((tmp_path / "resource-summary.json").read_text())
    assert saved["comparison"] == {}
    assert (tmp_path / "resource-report.txt").exists()


def test_coordinator_publishes_failure_before_leaving_capture_barrier(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    args = fixture(tmp_path / "template")
    args.output_dir = tmp_path / "run" / "agents"
    args.check_only = False
    args.prepare_capture = False
    args.capture_marker = tmp_path / "capture-ready"
    args.capture_timeout = 0
    args.startup_delay = 0
    args.payload_kb = 1
    args.base_url = "https://127.0.0.1:8443"
    args.run_id = "test"
    children = []

    class Child:
        def __init__(self, command: list[str]) -> None:
            self.index = int(command[command.index("--worker") + 1])
            self.stopped = self.index == 0
            (args.output_dir / f"ready-{self.index}.json").write_text(
                json.dumps({"pid": 100 + self.index})
            )
            for suffix in (".json", ".jsonl"):
                if suffix == ".json" and self.index == 0:
                    continue
                source = tmp_path / "template/agents" / f"agent-{self.index}{suffix}"
                (args.output_dir / source.name).write_bytes(source.read_bytes())
            children.append(self)

        def poll(self) -> int | None:
            return 1 if self.stopped else None

        def terminate(self) -> None:
            self.stopped = True

        def wait(self, timeout: float | None = None) -> int:
            assert self.stopped
            return 1

    monkeypatch.setattr(agents.subprocess, "Popen", Child)
    monkeypatch.setattr(agents.time, "monotonic", lambda: 10.0)
    assert agents.run(args) == 1
    saved = json.loads((args.output_dir.parent / "agent-summary.json").read_text())
    assert saved["passed"] is False
    assert saved["failed_agents"] == [0]
    assert "without publishing" in saved["error"]
    assert saved["agents"][1]["tokens_per_second"] == 100
    assert all(child.stopped for child in children)
    assert not args.capture_marker.exists()
