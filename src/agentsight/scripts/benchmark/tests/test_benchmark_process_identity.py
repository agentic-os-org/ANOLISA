"""Replacing the watched PID must not keep a benchmark's liveness gate green."""

import csv
import os
import sys
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from test_benchmark import collect_metrics, fault_injector, render_report


def proc_stat(pid: int, start_ticks: int) -> str:
    fields = ["R"] + ["0"] * 49
    fields[11], fields[12], fields[19] = "10", "20", str(start_ticks)
    return f"{pid} (worker with ) in its name) " + " ".join(fields)


def pin_stat(monkeypatch: pytest.MonkeyPatch, state: dict[str, Any]) -> int:
    pid = os.getpid()
    original = Path.read_text

    def read(path: Path, *args: Any, **kwargs: Any) -> str:
        if path == Path(f"/proc/{pid}/stat"):
            if state.get("unreadable"):
                raise PermissionError("fixture procfs unavailable")
            return proc_stat(pid, state["start"])
        return original(path, *args, **kwargs)

    monkeypatch.setattr(Path, "read_text", read)
    return pid


@pytest.mark.parametrize("replacement", [True, False])
def test_collector_main_tracks_one_process_lifetime(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, replacement: bool
) -> None:
    state = {"start": 100}
    pid = pin_stat(monkeypatch, state)
    monkeypatch.setattr(collect_metrics, "STOP", False)
    monkeypatch.setattr(collect_metrics.signal, "signal", lambda *args: None)
    iterations = 0

    def advance(_: float) -> None:
        nonlocal iterations
        iterations += 1
        if replacement:
            state["start"] = 200
        if iterations == 2:
            collect_metrics.STOP = True

    monkeypatch.setattr(collect_metrics.time, "sleep", advance)
    output = tmp_path / "metrics.csv"
    monkeypatch.setattr(
        sys, "argv", ["collect_metrics.py", "--pid", str(pid), "--output", str(output)]
    )
    assert collect_metrics.main() == 0
    with output.open() as handle:
        rows = list(csv.DictReader(handle))
    assert rows[0]["process_alive"] == "1"
    assert [row["process_alive"] for row in rows[1:]] == (["0", "0"] if replacement else ["1", "1"])
    if replacement:
        assert all(row["cpu_pct"] == row["rss_mb"] == "" for row in rows[1:])
        resources = render_report.summarize_resources(output)
        summary = render_report.build_summary(
            protocol="sse",
            qps="1",
            duration="2",
            load=render_report.summarize_load(None),
            resources=resources,
            drops=render_report.summarize_drops(output, 0),
            validation=None,
        )
        assert summary["process_survived"] is False


@pytest.mark.parametrize("replacement", [True, False])
def test_fault_injection_checks_the_original_process(
    monkeypatch: pytest.MonkeyPatch, replacement: bool
) -> None:
    state = {"start": 100}
    pid = pin_stat(monkeypatch, state)
    monkeypatch.setattr(fault_injector, "cases", lambda _: {})
    monkeypatch.setattr(
        fault_injector, "tls_socket", lambda *args: SimpleNamespace(close=lambda: None)
    )

    def healthy(*args: object) -> bool:
        if replacement:
            state["start"] = 200
        return True

    monkeypatch.setattr(fault_injector, "health_check", healthy)
    result = fault_injector.inject("localhost", 1, 1, 1, 1, pid)
    assert result["process_alive_before"] is True
    assert result["process_alive_after"] is (not replacement)


def test_fault_run_cannot_gain_a_process_identity_after_start(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    state = {"start": 100, "unreadable": True}
    pid = pin_stat(monkeypatch, state)
    monkeypatch.setattr(fault_injector, "cases", lambda _: {})
    monkeypatch.setattr(
        fault_injector, "tls_socket", lambda *args: SimpleNamespace(close=lambda: None)
    )

    def recover(*args: object) -> bool:
        state["unreadable"] = False
        return True

    monkeypatch.setattr(fault_injector, "health_check", recover)
    result = fault_injector.inject("localhost", 1, 1, 1, 1, pid)
    assert result["process_alive_before"] is False
    assert result["process_alive_after"] is False


def test_real_live_and_absent_process_controls() -> None:
    assert fault_injector.process_alive(os.getpid()) is True
    assert fault_injector.process_alive(30000000) is False
    assert fault_injector.process_alive(None) is None


@pytest.mark.parametrize("unreadable", [False, True])
def test_identity_change_during_resource_reads_discards_mixed_row(
    monkeypatch: pytest.MonkeyPatch, unreadable: bool
) -> None:
    state = {"start": 100}
    pid = pin_stat(monkeypatch, state)
    identity = collect_metrics.read_process_identity(pid)
    assert identity == collect_metrics.ProcessIdentity(pid, 100)
    original = collect_metrics.socket_count

    def replace_after_last_proc_read(target: int) -> int:
        count = original(target)
        state.update(start=200, unreadable=unreadable)
        return count

    monkeypatch.setattr(collect_metrics, "socket_count", replace_after_last_proc_read)
    monkeypatch.setattr(collect_metrics, "read_internal_metrics", lambda _: {"completed": "17"})
    row, ticks, _ = collect_metrics.sample(pid, 0, 0, 1, None, identity)
    assert row["process_alive"] == 0
    assert ticks is None
    assert all(
        row[field] == ""
        for field in ("cpu_pct", "rss_mb", "threads", "file_descriptors", "active_connections")
    )
    assert row["completed"] == "17"


def test_unreadable_initial_identity_preserves_existing_output(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    pid = pin_stat(monkeypatch, {"start": 100, "unreadable": True})
    output = tmp_path / "metrics.csv"
    output.write_text("previous measurements\n")
    monkeypatch.setattr(
        sys, "argv", ["collect_metrics.py", "--pid", str(pid), "--output", str(output)]
    )
    with pytest.raises(SystemExit, match="cannot identify process"):
        collect_metrics.main()
    assert output.read_text() == "previous measurements\n"


@pytest.mark.parametrize("text", ["", "123 (truncated)", proc_stat(123, -1)])
def test_invalid_start_time_is_not_an_identity(monkeypatch: pytest.MonkeyPatch, text: str) -> None:
    monkeypatch.setattr(Path, "read_text", lambda *args, **kwargs: text)
    assert collect_metrics.read_process_identity(123) is None


def test_fault_liveness_rechecks_identity_after_status(monkeypatch: pytest.MonkeyPatch) -> None:
    state = {"start": 100}
    pid = pin_stat(monkeypatch, state)
    identity = collect_metrics.read_process_identity(pid)

    def replaced(_: int) -> dict[str, int]:
        state["start"] = 200
        return {"VmRSS": 1024}

    monkeypatch.setattr(fault_injector, "read_status", replaced)
    assert fault_injector.process_alive(pid, identity) is False


def test_unwatched_fault_run_keeps_optional_liveness(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(fault_injector, "cases", lambda _: {})
    monkeypatch.setattr(
        fault_injector, "tls_socket", lambda *args: SimpleNamespace(close=lambda: None)
    )
    monkeypatch.setattr(fault_injector, "health_check", lambda *args: True)
    result = fault_injector.inject("localhost", 1, 1, 1, 1, None)
    assert result["process_alive_before"] is result["process_alive_after"] is None
