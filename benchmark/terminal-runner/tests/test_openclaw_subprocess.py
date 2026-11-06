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

"""Exercise the host subprocess runner without Harbor, Docker, or an LLM."""

from __future__ import annotations

import importlib.util
import os
import signal
import subprocess
import sys
import threading
import time
from collections.abc import Iterator
from pathlib import Path
from types import ModuleType
from typing import Any

import pytest

pytestmark = pytest.mark.skipif(os.name != "posix", reason="requires POSIX pipes and fork")


@pytest.fixture
def adapter(monkeypatch: pytest.MonkeyPatch) -> ModuleType:
    # Only the framework base types are stubbed; all subprocess I/O is real.
    path = Path(__file__).parents[1] / "external_agent" / "openclaw_external_agent.py"
    spec = importlib.util.spec_from_file_location("openclaw_subprocess_test_adapter", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    with monkeypatch.context() as imports:
        for name, symbol in (
            ("harbor.agents.base", "BaseAgent"),
            ("harbor.environments.base", "BaseEnvironment"),
            ("harbor.models.agent.context", "AgentContext"),
        ):
            stub = ModuleType(name)
            setattr(stub, symbol, type(symbol, (), {}))
            imports.setitem(sys.modules, name, stub)
        spec.loader.exec_module(module)
    monkeypatch.setattr(module.OpenClawExternalAgent, "_OPENCLAW_TIMEOUT_SEC", 1)
    monkeypatch.setenv("OPENCLAW_TIMEOUT", "1")
    monkeypatch.setenv("OPENCLAW_NO_OUTPUT_TIMEOUT", "10")
    monkeypatch.setenv("OPENCLAW_MAX_STDOUT_BYTES", "102400")
    return module


@pytest.fixture
def processes(
    adapter: ModuleType, monkeypatch: pytest.MonkeyPatch
) -> Iterator[list[subprocess.Popen[bytes]]]:
    original_popen = subprocess.Popen
    started: list[subprocess.Popen[bytes]] = []
    watchdogs: list[threading.Timer] = []

    def kill_group(proc: subprocess.Popen[bytes]) -> None:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass

    def popen(*args: Any, **kwargs: Any) -> subprocess.Popen[bytes]:
        # Isolate test descendants and bound even a regression to blocking read.
        proc = original_popen(*args, **kwargs, start_new_session=True)
        started.append(proc)
        watchdog = threading.Timer(4, kill_group, args=(proc,))
        watchdog.daemon = True
        watchdog.start()
        watchdogs.append(watchdog)
        return proc

    monkeypatch.setattr(adapter.subprocess, "Popen", popen)
    yield started
    for watchdog in watchdogs:
        watchdog.cancel()
        watchdog.join()
    for proc in started:
        kill_group(proc)
        proc.wait(timeout=5)
        for stream in (proc.stdout, proc.stderr):
            if stream is not None:
                stream.close()


@pytest.mark.parametrize("held_fd", [1, 2])
def test_inherited_pipe_respects_total_timeout(
    adapter: ModuleType,
    processes: list[subprocess.Popen[bytes]],
    caplog: pytest.LogCaptureFixture,
    held_fd: int,
) -> None:
    code = f"""
import os, time
if os.fork() == 0:
    os.close({3 - held_fd})
    time.sleep(30)
    os._exit(0)
os.write(1, b'parent stdout\\n')
os.write(2, b'parent stderr\\n')
time.sleep(0.05)
os._exit(0)
"""
    start = time.monotonic()
    result = adapter.OpenClawExternalAgent._sync_run_openclaw([sys.executable, "-c", code])
    elapsed = time.monotonic() - start
    assert elapsed < 2.5, f"inherited pipe blocked for {elapsed:.2f}s"
    assert "subprocess timeout" in caplog.text
    assert result == (0, "parent stdout\n", "parent stderr\n")
    assert processes[0].stdout.closed
    assert processes[0].stderr.closed


def test_collects_delayed_output_after_parent_exit(
    adapter: ModuleType, processes: list[subprocess.Popen[bytes]]
) -> None:
    code = """
import os, time
if os.fork() == 0:
    time.sleep(0.2)
    os.write(1, b'x' * 20000)
    os.write(2, b'y' * 20000)
    os._exit(0)
os.write(1, b'prefix')
os._exit(7)
"""
    result = adapter.OpenClawExternalAgent._sync_run_openclaw([sys.executable, "-c", code])
    assert result == (7, "prefix" + "x" * 20000, "y" * 20000)


def test_inherited_pipe_respects_no_output_timeout(
    adapter: ModuleType,
    processes: list[subprocess.Popen[bytes]],
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
) -> None:
    monkeypatch.setenv("OPENCLAW_TIMEOUT", "10")
    monkeypatch.setenv("OPENCLAW_NO_OUTPUT_TIMEOUT", "1")
    code = """
import os, time
if os.fork() == 0:
    time.sleep(30)
os._exit(0)
"""
    start = time.monotonic()
    result = adapter.OpenClawExternalAgent._sync_run_openclaw([sys.executable, "-c", code])
    assert time.monotonic() - start < 2.5
    assert result == (0, "", "")
    assert "no stdout" in caplog.text


def test_descendant_output_respects_stdout_cap(
    adapter: ModuleType,
    processes: list[subprocess.Popen[bytes]],
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
) -> None:
    monkeypatch.setenv("OPENCLAW_TIMEOUT", "10")
    monkeypatch.setenv("OPENCLAW_MAX_STDOUT_BYTES", "16384")
    code = """
import os, time
if os.fork() == 0:
    time.sleep(0.2)
    while True:
        os.write(1, b'x' * 8192)
os._exit(0)
"""
    start = time.monotonic()
    _, stdout, _ = adapter.OpenClawExternalAgent._sync_run_openclaw([sys.executable, "-c", code])
    assert time.monotonic() - start < 2.5
    assert 16384 < len(stdout) <= 16384 + 8192
    assert "stdout exceeded" in caplog.text


def test_eof_does_not_skip_waiting_for_process(
    adapter: ModuleType, processes: list[subprocess.Popen[bytes]]
) -> None:
    code = "import os, time; os.close(1); os.close(2); time.sleep(0.2); os._exit(7)"
    result = adapter.OpenClawExternalAgent._sync_run_openclaw([sys.executable, "-c", code])
    assert result == (7, "", "")


def test_collects_large_output_and_exit_status(
    adapter: ModuleType, processes: list[subprocess.Popen[bytes]]
) -> None:
    code = "import os; os.write(1, b'x' * 50000); os.write(2, b'y' * 50000); os._exit(7)"
    result = adapter.OpenClawExternalAgent._sync_run_openclaw([sys.executable, "-c", code])
    assert result == (7, "x" * 50000, "y" * 50000)


@pytest.mark.parametrize("has_output", [False, True])
def test_timeout_still_kills_running_process(
    adapter: ModuleType,
    processes: list[subprocess.Popen[bytes]],
    monkeypatch: pytest.MonkeyPatch,
    has_output: bool,
) -> None:
    if not has_output:
        monkeypatch.setenv("OPENCLAW_TIMEOUT", "10")
        monkeypatch.setenv("OPENCLAW_NO_OUTPUT_TIMEOUT", "1")
    code = "import os, time; " + ("os.write(1, b'hello'); " if has_output else "")
    code += "time.sleep(30)"
    start = time.monotonic()
    returncode, stdout, stderr = adapter.OpenClawExternalAgent._sync_run_openclaw(
        [sys.executable, "-c", code]
    )
    assert time.monotonic() - start < 2.5
    assert returncode == -signal.SIGKILL
    assert stdout == ("hello" if has_output else "")
    assert stderr == ""
