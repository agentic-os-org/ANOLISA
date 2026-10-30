"""An unreaped process is no longer running a benchmark workload."""

import os
import time
from collections.abc import Iterator
from pathlib import Path

import pytest
from test_benchmark import collect_metrics, fault_injector

pytestmark = pytest.mark.skipif(not hasattr(os, "fork"), reason="Linux procfs fixture")


@pytest.fixture
def zombie() -> Iterator[int]:
    pid = os.fork()
    if pid == 0:
        os._exit(0)
    try:
        deadline = time.monotonic() + 3
        while "State:\tZ" not in Path(f"/proc/{pid}/status").read_text():
            assert time.monotonic() < deadline
            time.sleep(0.01)
        yield pid
    finally:
        os.waitpid(pid, 0)


def test_sampler_marks_real_zombie_as_stopped(zombie: int) -> None:
    row, _, _ = collect_metrics.sample(zombie, 0, time.monotonic() - 1, 1, None)
    assert row["process_alive"] == 0
    assert row["cpu_pct"] == row["rss_mb"] == ""


def test_fault_gate_rejects_real_zombie(zombie: int) -> None:
    assert fault_injector.process_alive(zombie) is False


def test_live_and_absent_process_controls() -> None:
    assert fault_injector.process_alive(os.getpid()) is True
    assert fault_injector.process_alive(30000000) is False
    assert fault_injector.process_alive(None) is None
