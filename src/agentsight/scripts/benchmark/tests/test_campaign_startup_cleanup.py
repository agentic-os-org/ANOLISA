"""A context whose startup raises must release its owned child and log."""

import signal
import subprocess
import sys
import time
from pathlib import Path
from unittest.mock import patch

import pytest
from test_campaign_runner import run_campaign


@pytest.mark.parametrize("kind", ["tracer", "server"])
@pytest.mark.parametrize("failure", [OSError("fixture readiness failure"), KeyboardInterrupt()])
def test_startup_failure_reaps_owned_child(
    tmp_path: Path, kind: str, failure: BaseException
) -> None:
    child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
    if kind == "tracer":
        owned = run_campaign.TracerProcess(
            {"binary": "unused", "config": "unused", "log_file": str(tmp_path / "trace.log")}, 3
        )
    else:
        owned = run_campaign.BenchmarkServer(
            {"chunks": 1, "chunk_bytes": 1, "protocol": "sse"}, tmp_path, startup_timeout=3
        )
    calls = 0
    real_sleep = time.sleep

    def interrupt_once(seconds: float) -> None:
        nonlocal calls
        calls += 1
        if calls == 1:
            raise failure
        real_sleep(seconds)

    def run(*args: object, **kwargs: object) -> subprocess.CompletedProcess:
        nonlocal calls
        calls += 1
        if calls == 1:
            return subprocess.CompletedProcess([], 0)
        raise failure

    try:
        with patch.object(run_campaign.subprocess, "Popen", return_value=child):
            if kind == "tracer":
                with patch.object(run_campaign.time, "sleep", side_effect=interrupt_once):
                    with pytest.raises(type(failure)):
                        owned.__enter__()
            else:
                with patch.object(run_campaign.subprocess, "run", side_effect=run):
                    with pytest.raises(type(failure)):
                        owned.__enter__()
        assert child.poll() is not None, "startup escaped without stopping its child"
        assert owned.log is not None and owned.log.closed
    finally:
        if child.poll() is None:
            child.send_signal(signal.SIGTERM)
        child.wait(timeout=5)
        owned.stop()


def test_spawn_failure_closes_tracer_log(tmp_path: Path) -> None:
    owned = run_campaign.TracerProcess(
        {"binary": "unused", "config": "unused", "log_file": str(tmp_path / "trace.log")}, 3
    )
    try:
        with patch.object(run_campaign.subprocess, "Popen", side_effect=OSError("fixture spawn")):
            with pytest.raises(OSError):
                owned.__enter__()
        assert owned.log is not None and owned.log.closed
    finally:
        owned.stop()
