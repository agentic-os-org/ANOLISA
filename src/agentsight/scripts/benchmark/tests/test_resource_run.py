"""Exercise orchestration with real HTTPS load and explicitly simulated capture."""

from __future__ import annotations

import csv
import importlib.metadata
import importlib.util
import json
import os
import pty
import shutil
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

import pytest

SINGLE_RUN = Path(__file__).parents[1] / "single_run"
sys.path.insert(0, str(SINGLE_RUN))
import resource_inputs
import resource_report

FAKE_TRACE = """#!/usr/bin/env python3
import json, os, signal, sqlite3, sys, time
from pathlib import Path
configuration = json.loads(Path(sys.argv[sys.argv.index("--config") + 1]).read_text())
directory = Path(configuration["storage"]["base_path"]).parent
directory.joinpath("db").mkdir()
connection = sqlite3.connect(directory / "db/genai_events.db")
connection.execute("CREATE TABLE genai_events (id INTEGER PRIMARY KEY, call_id TEXT, status TEXT, total_tokens INTEGER)")
connection.commit()
allocation = bytearray(MEMORY_MIB * 1024 * 1024)
job = json.loads((directory / "job.json").read_text())
total = int(job["load_args"][job["load_args"].index("--tokens-per-response") + 1]) + 12
prepared = json.loads((directory / "clients-ready.json").read_text())
assert len(prepared["pids"]) == int(job["load_args"][job["load_args"].index("--agents") + 1])
assert all(Path(f"/proc/{pid}").exists() for pid in prepared["pids"])
assert not (directory / "agents/start.json").exists()
(directory / "observer-start.json").write_text(json.dumps(prepared))
mode = os.environ.get("MEMORY_TEST_MODE", "normal")
if mode in ("ring-drop", "channel-drop", "late-ring-drop"):
    Path(os.environ["AGENTSIGHT_METRICS_FILE"]).write_text(
        "agentsight_ring_buffer_dropped_total 0\\n" +
        "agentsight_channel_dropped_total 0\\n")
if mode == "startup-failure":
    raise SystemExit(7)
if mode != "timeout":
    print("AgentSight initialized: fake observer for orchestration tests", flush=True)
seen = set()
capture_delayed = False
probe_drop_written = False
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
signal.signal(signal.SIGINT, lambda *_: sys.exit(0))
while True:
    if mode == "crash" and (directory / "agents/start.json").exists():
        raise SystemExit(8)
    if mode in ("ring-drop", "channel-drop") and not probe_drop_written and (directory / "agents/start.json").exists():
        window = json.loads((directory / "agents/start.json").read_text())
        if time.monotonic() >= window["start"] + 0.3:
            ring = 1 if mode == "ring-drop" else 0
            channel = 1 if mode == "channel-drop" else 0
            Path(os.environ["AGENTSIGHT_METRICS_FILE"]).write_text(
                f"agentsight_ring_buffer_dropped_total {ring}\\n" +
                f"agentsight_channel_dropped_total {channel}\\n")
            probe_drop_written = True
    for path in directory.glob("request-logs/*.jsonl"):
        for line in path.read_text().splitlines():
            record = json.loads(line)
            request_id = record["request_id"]
            if request_id not in seen:
                preflight = request_id.startswith("preflight-")
                if mode in ("delayed-capture", "late-ring-drop") and not preflight:
                    if not (directory / "agent-summary.json").exists():
                        continue
                    if not capture_delayed:
                        time.sleep(0.25)
                        if mode == "late-ring-drop":
                            Path(os.environ["AGENTSIGHT_METRICS_FILE"]).write_text(
                                "agentsight_ring_buffer_dropped_total 1\\n" +
                                "agentsight_channel_dropped_total 0\\n")
                        capture_delayed = True
                seen.add(request_id)
                if mode == "delayed-capture" and not preflight:
                    agent = request_id.rsplit("-", 2)[1]
                    pid = json.loads((directory / f"agents/ready-{agent}.json").read_text())["pid"]
                    if not Path(f"/proc/{pid}").exists():
                        print("discarded capture after client exit", flush=True)
                        continue
                if preflight and (mode == "preflight-missing" or (mode == "preflight-one-missing" and request_id.endswith("-1"))):
                    continue
                if preflight or mode != "missing":
                    tokens = 13 if preflight else total + (1 if mode == "wrong-tokens" else 0)
                    if preflight and mode == "preflight-wrong-tokens":
                        tokens += 1
                    status = "pending" if preflight and mode == "preflight-incomplete" else "complete"
                    connection.execute("INSERT INTO genai_events(call_id, status, total_tokens) VALUES (?, ?, ?)",
                                       (request_id, status, tokens))
                    connection.commit()
    time.sleep(0.02)
"""


def git(repo: Path, *arguments: str) -> str:
    return subprocess.check_output(["git", "-C", str(repo), *arguments], text=True).strip()


def executable(path: Path, content: str) -> Path:
    path.write_text(content)
    path.chmod(0o755)
    return path


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


@pytest.fixture
def environment(tmp_path: Path) -> dict[str, str]:
    cache = tmp_path / "cache/agentsight-benchmark/tiktoken"
    cache.mkdir(parents=True)
    import tiktoken

    tiktoken.get_encoding("cl100k_base")
    existing = Path(os.environ.get("TIKTOKEN_CACHE_DIR", "/tmp/data-gym-cache"))
    if existing.exists():
        shutil.copytree(existing, cache, dirs_exist_ok=True)
    return dict(os.environ, XDG_CACHE_HOME=str(tmp_path / "cache"), PYTHON=sys.executable)


@pytest.fixture
def sudo_environment(
    tmp_path: Path, environment: dict[str, str], isolated_runtime: Path
) -> dict[str, str]:
    if os.geteuid() == 0:
        pytest.skip("sudo authentication requires an unprivileged parent")
    tools = tmp_path / "tools"
    tools.mkdir()
    (tmp_path / "cache/agentsight-benchmark/venv").symlink_to(
        isolated_runtime, target_is_directory=True
    )
    executable(
        tools / "sudo",
        """#!/usr/bin/env python3
import json, os, signal, subprocess, sys
from pathlib import Path
cache = Path(os.environ["MEMORY_TEST_SUDO_CACHE"])
with cache.with_suffix(".commands").open("a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\\n")
scope = str(os.getsid(0))
if sys.argv[1:] == ["-v"]:
    cache.write_text(scope)
    raise SystemExit(0)
if not cache.exists() or cache.read_text() != scope:
    print("sudo: a password is required", file=sys.stderr)
    raise SystemExit(1)
assert sys.argv[1:3] == ["-n", "--"]
if sys.argv[3] == "kill":
    raise SystemExit(89)
child_env = dict(os.environ, HOME=str(cache.parent / "root-home"), PYTHONNOUSERSITE="1")
child_env.pop("PYTHONPATH", None)
child_env.pop("PYTHON", None)
child = subprocess.Popen(sys.argv[3:], env=child_env)
signal.signal(signal.SIGTERM, lambda signum, _: child.send_signal(signum))
raise SystemExit(child.wait())
""",
    )
    return dict(
        environment,
        PATH=f"{tools}:{environment['PATH']}",
        MEMORY_TEST_SUDO_CACHE=str(tmp_path / "sudo-cache"),
    )


@pytest.fixture(scope="session")
def isolated_runtime(tmp_path_factory: pytest.TempPathFactory) -> Path:
    runtime = tmp_path_factory.mktemp("memory-python") / "venv"
    subprocess.run([sys.executable, "-m", "venv", "--without-pip", str(runtime)], check=True)
    packages = Path(
        subprocess.check_output(
            [
                str(runtime / "bin/python3"),
                "-c",
                "import sysconfig; print(sysconfig.get_path('purelib'))",
            ],
            text=True,
        ).strip()
    )
    # Copy installed test dependencies instead of requiring a network download.
    for name in (
        "tiktoken",
        "tiktoken_ext",
        "regex",
        "requests",
        "urllib3",
        "idna",
        "charset_normalizer",
        "certifi",
    ):
        specification = importlib.util.find_spec(name)
        assert specification is not None, f"test dependency is missing: {name}"
        for location in specification.submodule_search_locations or ():
            shutil.copytree(location, packages / name, dirs_exist_ok=True)
    distribution = importlib.metadata.distribution("tiktoken")
    metadata = next(
        file
        for file in distribution.files or ()
        if file.name == "METADATA" and file.parent.suffix == ".dist-info"
    )
    metadata_directory = Path(distribution.locate_file(metadata)).parent
    shutil.copytree(metadata_directory, packages / metadata_directory.name)
    return runtime


def command(output: Path, binary: Path, script: Path = SINGLE_RUN / "resource_run.sh") -> list[str]:
    return [
        "bash",
        str(script),
        "--agentsight-bin",
        str(binary),
        "--no-sudo",
        "--agents",
        "2",
        "--tokens-per-second",
        "100",
        "--tokens-per-response",
        "100",
        "--duration",
        "2",
        "--startup-delay",
        "0",
        "--rate-tolerance",
        "0.15",
        "--validation-wait-seconds",
        "1",
        "--port",
        str(free_port()),
        "--min-free-disk-gb",
        "1",
        "--min-available-memory-mb",
        "1",
        "--output-dir",
        str(output),
    ]


def run(
    args: list[str], env: dict[str, str], *, interactive: bool = False
) -> subprocess.CompletedProcess[str]:
    if not interactive:
        return subprocess.run(
            args, env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=40
        )
    master, slave = pty.openpty()
    try:
        return subprocess.run(
            args, env=env, stdin=slave, capture_output=True, text=True, timeout=40
        )
    finally:
        os.close(slave)
        os.close(master)


def assert_reaped(output: Path) -> None:
    for state_path in output.glob("*/worker-pids.json"):
        state = json.loads(state_path.read_text())
        for pid in [state["worker"], *state["groups"]]:
            assert not Path(f"/proc/{pid}").exists(), f"owned process {pid} survived"
    for ready_path in output.glob("*/agents/ready-*.json"):
        pid = json.loads(ready_path.read_text())["pid"]
        assert not Path(f"/proc/{pid}").exists(), f"agent {pid} survived"


@pytest.mark.parametrize(
    "capacity,ring_buffer,configured_ring,cpu_budget,configured_cpu",
    [
        (None, None, 32, None, None),
        (48, 64, 32, None, None),
        (None, None, 128, None, None),
        (None, None, 32, 25, 40),
        (None, None, 32, 0, None),
        (None, None, 32, None, 0),
        (None, None, 32, None, 40),
    ],
    ids=[
        "configured-defaults",
        "explicit-capacity-and-ring",
        "preserve-configured-ring",
        "cpu-paced",
        "explicit-cpu-disabled",
        "configured-cpu-disabled",
        "preserve-configured-cpu",
    ],
)
def test_one_command_reports_real_rss_and_cleans_processes(
    tmp_path: Path,
    environment: dict[str, str],
    capacity: int | None,
    ring_buffer: int | None,
    configured_ring: int,
    cpu_budget: int | None,
    configured_cpu: int | None,
) -> None:
    binary = executable(tmp_path / "observer", FAKE_TRACE.replace("MEMORY_MIB", "16"))
    output = tmp_path / "result"
    args = command(output, binary)
    # Start the collector before the load and retain several CPU sampling intervals.
    args[args.index("--startup-delay") + 1] = "1"
    args[args.index("--duration") + 1] = "4"
    configuration = json.loads((SINGLE_RUN.parents[2] / "agentsight.json").read_text())
    configuration["runtime_limits"]["ring_buffer_mb"] = configured_ring
    if configured_cpu is not None:
        configuration["runtime_limits"]["cpu_budget_percent"] = configured_cpu
    config_path = tmp_path / "config.json"
    config_path.write_text(json.dumps(configuration))
    args.extend(["--config", str(config_path)])
    if capacity is not None:
        args.extend(["--connection-capacity", str(capacity)])
    if ring_buffer is not None:
        args.extend(["--ring-buffer-mb", str(ring_buffer)])
    if cpu_budget is not None:
        args.extend(["--cpu-budget-percent", str(cpu_budget)])
    result = run(args, environment)
    assert result.returncode == 0, result.stdout + result.stderr
    summary = json.loads((output / "resource-summary.json").read_text())
    measured = summary["runs"]["run"]
    assert summary["valid"]
    expected_cpu = cpu_budget if cpu_budget is not None else configured_cpu
    if expected_cpu is None:
        expected_cpu = 25
    assert summary["settings"]["cpu_budget_percent"] == expected_cpu
    applied = json.loads((output / "run/config.json").read_text())["runtime_limits"]
    assert applied["cpu_budget_percent"] == expected_cpu
    if expected_cpu:
        assert f"Configured CPU pacing target: {expected_cpu}%" in result.stdout
    else:
        assert "Configured CPU pacing target:" not in result.stdout
    assert measured["load"] == measured["capture"] == "PASS"
    prepared = json.loads((output / "run/clients-ready.json").read_text())
    assert len(prepared["pids"]) == 2
    assert json.loads((output / "run/observer-start.json").read_text()) == prepared
    readiness = json.loads((output / "run/capture-readiness.json").read_text())
    assert readiness["ready"] and readiness["token_correct"] == readiness["workers"] == 2
    assert (output / "run/agents/start.json").stat().st_mtime_ns >= (
        output / "run/capture-ready.json"
    ).stat().st_mtime_ns
    assert all(not request_id.startswith("bench-") for request_id in prepared["request_ids"])
    assert measured["rss_peak_mib"] >= measured["rss_average_mib"] > 16
    assert measured["cpu_average_pct"] >= 0
    # Time-weighted sums may exceed a constant sample by a floating-point ULP.
    assert measured["cpu_average_pct"] <= measured["cpu_peak_pct"] + 1e-9
    assert measured["cpu_samples"] > 0
    assert measured["cpu_measurement_seconds"] == pytest.approx(4, abs=0.2)
    assert measured["tail"]["cpu_samples"] > 0
    assert "Average CPU" in result.stdout and "P95 CPU" in result.stdout
    assert "100% = one logical CPU" in result.stdout
    assert measured["rss_peak_host_pct"] == pytest.approx(
        measured["rss_peak_mib"] / measured["host_memory_mib"] * 100
    )
    assert "Average RSS" in result.stdout and "Peak / host RAM" in result.stdout
    assert "Load target: 2 agents x 100 output tok/s, 4s" in result.stdout
    expected_capacity = capacity or 2
    assert f"Configured connection capacity: {expected_capacity}" in result.stdout
    assert summary["settings"]["connection_capacity"] == expected_capacity
    expected_ring = ring_buffer or configured_ring
    assert f"Configured ring buffer: {expected_ring} MiB" in result.stdout
    assert summary["settings"]["ring_buffer_mb"] == expected_ring
    assert (
        json.loads((output / "run/config.json").read_text())["runtime_limits"][
            "connection_capacity"
        ]
        == expected_capacity
    )
    assert (
        json.loads((output / "run/config.json").read_text())["runtime_limits"]["ring_buffer_mb"]
        == expected_ring
    )
    assert not summary["runs"]["run"]["probe_drops"]["ring_buffer_dropped"]
    assert (output / "resource-report.txt").read_text() in result.stdout
    assert_reaped(output)
    reused = run(command(output, binary), environment)
    assert reused.returncode != 0 and "already exists" in reused.stderr


@pytest.mark.parametrize(
    "mode,correct",
    [
        ("preflight-missing", 0),
        ("preflight-one-missing", 1),
        ("preflight-wrong-tokens", 0),
        ("preflight-incomplete", 0),
    ],
)
def test_capture_readiness_failure_never_starts_the_measurement(
    tmp_path: Path,
    environment: dict[str, str],
    mode: str,
    correct: int,
) -> None:
    binary = executable(tmp_path / "observer", FAKE_TRACE.replace("MEMORY_MIB", "1"))
    output = tmp_path / "result"
    result = run(
        command(output, binary) + ["--duration", "30", "--ready-timeout", "1"],
        dict(environment, MEMORY_TEST_MODE=mode),
    )
    assert result.returncode != 0
    assert "TLS capture readiness timed out" in result.stdout
    readiness = json.loads((output / "run/capture-readiness.json").read_text())
    assert readiness["workers"] == 2 and readiness["token_correct"] == correct
    assert not readiness["ready"]
    assert not (output / "run/agents/start.json").exists()
    assert not (output / "run/metrics.csv").exists()
    assert not (output / "run/agent-summary.json").exists()
    summary = json.loads((output / "resource-summary.json").read_text())
    assert not summary["valid"] and not summary["comparison"]
    assert summary["runs"]["run"]["capture"] == "FAIL"
    assert summary["runs"]["run"]["rss_peak_mib"] is None
    assert_reaped(output)


def test_interrupt_during_capture_preparation_reaps_waiting_clients(
    tmp_path: Path,
    environment: dict[str, str],
) -> None:
    binary = executable(tmp_path / "observer", FAKE_TRACE.replace("MEMORY_MIB", "1"))
    output = tmp_path / "result"
    process = subprocess.Popen(
        command(output, binary) + ["--ready-timeout", "30"],
        env=dict(environment, MEMORY_TEST_MODE="preflight-missing"),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 15
        while not (output / "run/agents/preflight-1.json").exists():
            assert process.poll() is None
            assert time.monotonic() < deadline
            time.sleep(0.05)
        assert not (output / "run/agents/start.json").exists()
        process.send_signal(signal.SIGINT)
        stdout, stderr = process.communicate(timeout=20)
        assert process.returncode == 130, stdout + stderr
        assert (
            json.loads((output / "run/worker-result.json").read_text())["status"] == "interrupted"
        )
        assert_reaped(output)
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()


def test_delayed_capture_finishes_before_clients_exit(
    tmp_path: Path, environment: dict[str, str]
) -> None:
    binary = executable(tmp_path / "observer", FAKE_TRACE.replace("MEMORY_MIB", "1"))
    output = tmp_path / "result"
    result = run(
        command(output, binary) + ["--payload-kb", "440"],
        dict(environment, MEMORY_TEST_MODE="delayed-capture"),
    )
    assert result.returncode == 0, result.stdout + result.stderr
    summary = json.loads((output / "resource-summary.json").read_text())
    assert summary["valid"]
    assert summary["runs"]["run"]["capture"] == "PASS"
    counts = summary["runs"]["run"]["capture_counts"]
    assert counts["complete"] == counts["sent"] == counts["token_correct"]
    assert counts["sent"] >= 4
    assert "discarded capture" not in (output / "run/trace.log").read_text()
    assert_reaped(output)


@pytest.mark.parametrize("mode", ["ring-drop", "channel-drop", "late-ring-drop"])
def test_probe_drops_invalidate_complete_request_capture(
    tmp_path: Path, environment: dict[str, str], mode: str
) -> None:
    binary = executable(tmp_path / "observer", FAKE_TRACE.replace("MEMORY_MIB", "1"))
    output = tmp_path / "result"
    result = run(command(output, binary), dict(environment, MEMORY_TEST_MODE=mode))
    assert result.returncode != 0, result.stdout + result.stderr
    summary = json.loads((output / "resource-summary.json").read_text())
    capture = json.loads((output / "run/report.json").read_text())
    assert capture["complete"] == capture["sent"] and capture["token_accuracy"] == 1
    assert summary["runs"]["run"]["load"] == "PASS"
    assert summary["runs"]["run"]["capture"] == "FAIL" and not summary["valid"]
    key = "channel_dropped" if mode == "channel-drop" else "ring_buffer_dropped"
    assert summary["runs"]["run"]["probe_drops"][key] == 1
    assert "capture failed: probe events were dropped" in result.stdout
    assert_reaped(output)


def test_ab_with_probe_drops_suppresses_memory_improvement(
    tmp_path: Path, environment: dict[str, str]
) -> None:
    before = executable(tmp_path / "before", FAKE_TRACE.replace("MEMORY_MIB", "32"))
    after = executable(tmp_path / "after", FAKE_TRACE.replace("MEMORY_MIB", "1"))
    output = tmp_path / "result"
    result = run(
        command(output, after) + ["--ab", "--baseline-bin", str(before)],
        dict(environment, MEMORY_TEST_MODE="ring-drop"),
    )
    assert result.returncode != 0, result.stdout + result.stderr
    summary = json.loads((output / "resource-summary.json").read_text())
    assert not summary["valid"] and not summary["comparison"]
    assert all(run["capture_counts"]["complete"] > 0 for run in summary["runs"].values())
    assert all(run["capture"] == "FAIL" for run in summary["runs"].values())
    assert "N/A (invalid)" in result.stdout
    assert_reaped(output)


def test_host_memory_guard_reports_threshold_and_cleans_processes(
    tmp_path: Path, environment: dict[str, str]
) -> None:
    binary = executable(tmp_path / "observer", FAKE_TRACE.replace("MEMORY_MIB", "1"))
    output = tmp_path / "result"
    total_line = next(
        line
        for line in Path("/proc/meminfo").read_text().splitlines()
        if line.startswith("MemTotal:")
    )
    threshold = int(total_line.split()[1]) // 1024 + 1
    result = run(
        command(output, binary) + ["--min-available-memory-mb", str(threshold)], environment
    )
    assert result.returncode != 0, result.stdout + result.stderr
    summary = json.loads((output / "resource-summary.json").read_text())
    stop = json.loads((output / "run/safety-stop.json").read_text())
    assert stop["reason"] == "available_memory"
    assert summary["runs"]["run"]["safety_stop"] == stop
    assert not summary["valid"] and not summary["comparison"]
    assert "run stopped by resource guard: host available RAM" in result.stdout
    assert f"< {threshold:.2f} MiB" in result.stdout
    assert_reaped(output)


@pytest.mark.parametrize(
    "record,message",
    [
        (
            {"reason": "available_memory", "observed": 1915854848, "limit": 2147483648},
            "host available RAM 1827.10 MiB < 2048.00 MiB",
        ),
        (
            {"reason": "agentsight_rss", "observed": 1537.5, "limit": 1536},
            "AgentSight RSS 1537.50 MiB > 1536.00 MiB",
        ),
        (
            {"reason": "free_disk", "observed": 1024**3, "limit": 5 * 1024**3},
            "free disk 1.00 GiB < 5.00 GiB",
        ),
        (
            {"reason": "results_budget", "observed": 3 * 1024**3, "limit": 2 * 1024**3},
            "results size 3.00 GiB >= 2.00 GiB",
        ),
        ({"reason": "future_guard"}, "future_guard"),
        ({"reason": "available_memory"}, "available_memory"),
    ],
)
def test_guard_record_invalidates_report_and_preserves_reason(
    tmp_path: Path, record: dict[str, object], message: str, capsys: pytest.CaptureFixture[str]
) -> None:
    directory = tmp_path / "run"
    directory.mkdir()
    (directory / "worker-result.json").write_text(
        json.dumps({"status": "complete", "returncode": 0, "trace_survived": True})
    )
    (directory / "agent-summary.json").write_text(json.dumps({"passed": True}))
    (directory / "report.json").write_text(
        json.dumps({"sent": 1, "completeness_ratio": 1, "token_accuracy": 1})
    )
    (directory / "metrics.csv").write_text("process_alive,rss_mb\n1,100\n")
    (directory / "safety-stop.json").write_text(json.dumps(record))
    assert not resource_report.report(tmp_path, {"run": {"path": "test-observer"}})
    assert f"run stopped by resource guard: {message}" in capsys.readouterr().out
    summary = json.loads((tmp_path / "resource-summary.json").read_text())
    assert summary["runs"]["run"]["safety_stop"] == record
    assert not summary["valid"] and not summary["comparison"]


@pytest.mark.parametrize("mode", ["missing", "wrong-tokens", "startup-failure", "timeout", "crash"])
def test_failures_are_invalid_and_cleaned(
    tmp_path: Path, environment: dict[str, str], mode: str
) -> None:
    binary = executable(tmp_path / "observer", FAKE_TRACE.replace("MEMORY_MIB", "1"))
    output = tmp_path / "result"
    result = run(
        command(output, binary) + ["--ready-timeout", "1"], dict(environment, MEMORY_TEST_MODE=mode)
    )
    assert result.returncode != 0
    summary = json.loads((output / "resource-summary.json").read_text())
    assert not summary["valid"] and not summary["comparison"]
    if mode in ("missing", "wrong-tokens"):
        assert summary["runs"]["run"]["capture"] == "FAIL"
    else:
        assert summary["runs"]["run"]["status"] == "failed"
        assert "trace.log" in result.stdout
    assert_reaped(output)


@pytest.mark.parametrize("signum", [signal.SIGINT, signal.SIGTERM])
@pytest.mark.parametrize("phase", ["load", "capture-drain"])
def test_interrupt_preserves_report_and_reaps_agents(
    tmp_path: Path, environment: dict[str, str], signum: int, phase: str
) -> None:
    binary = executable(tmp_path / "observer", FAKE_TRACE.replace("MEMORY_MIB", "1"))
    output = tmp_path / "result"
    draining = phase == "capture-drain"
    process = subprocess.Popen(
        command(output, binary)
        + ["--duration", "1" if draining else "30", "--validation-wait-seconds", "30"],
        env=dict(environment, MEMORY_TEST_MODE="missing" if draining else "normal"),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 15
        marker = output / ("run/agent-summary.json" if draining else "run/agents/start.json")
        while not marker.exists():
            if process.poll() is not None or time.monotonic() >= deadline:
                pytest.fail("load did not start: " + str(process.communicate(timeout=2)))
            time.sleep(0.05)
        process.send_signal(signum)
        stdout, stderr = process.communicate(timeout=20)
        assert process.returncode == 128 + signum, stdout + stderr
        summary = json.loads((output / "resource-summary.json").read_text())
        assert not summary["valid"]
        assert summary["runs"]["run"]["status"] == "interrupted"
        assert_reaped(output)
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()


def test_ab_prebuilt_binaries_report_reductions(
    tmp_path: Path, environment: dict[str, str]
) -> None:
    before = executable(tmp_path / "before", FAKE_TRACE.replace("MEMORY_MIB", "32"))
    after = executable(tmp_path / "after", FAKE_TRACE.replace("MEMORY_MIB", "4"))
    output = tmp_path / "result"
    result = run(
        command(output, after) + ["--ab", "--baseline-bin", str(before), "--ring-buffer-mb", "64"],
        environment,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    summary = json.loads((output / "resource-summary.json").read_text())
    assert summary["valid"] and list(summary["runs"]) == ["before", "after"]
    change = summary["comparison"]["rss_peak_mib"]
    assert change["mib"] > 20 and change["percent"] > 30
    assert (
        json.loads((output / "before/config.json").read_text())["runtime_limits"]
        == json.loads((output / "after/config.json").read_text())["runtime_limits"]
    )
    assert summary["settings"]["ring_buffer_mb"] == 64
    with (output / "before/metrics.csv").open() as before_file, (
        output / "after/metrics.csv"
    ).open() as after_file:
        before_samples, after_samples = list(csv.DictReader(before_file)), list(
            csv.DictReader(after_file)
        )
    assert max(float(row["timestamp"]) for row in before_samples) < min(
        float(row["timestamp"]) for row in after_samples
    )
    assert_reaped(output)


def test_ab_does_not_publish_improvement_when_one_capture_fails(
    tmp_path: Path, environment: dict[str, str]
) -> None:
    before = executable(
        tmp_path / "before",
        FAKE_TRACE.replace("MEMORY_MIB", "32").replace(
            '"MEMORY_TEST_MODE", "normal"', '"MEMORY_TEST_MODE", "missing"'
        ),
    )
    after = executable(tmp_path / "after", FAKE_TRACE.replace("MEMORY_MIB", "4"))
    output = tmp_path / "result"
    result = run(command(output, after) + ["--ab", "--baseline-bin", str(before)], environment)
    assert result.returncode == 1
    summary = json.loads((output / "resource-summary.json").read_text())
    assert summary["runs"]["before"]["capture"] == "FAIL"
    assert summary["runs"]["after"]["capture"] == "PASS"
    assert not summary["valid"] and not summary["comparison"]
    assert "N/A (invalid)" in result.stdout
    assert_reaped(output)


@pytest.mark.parametrize("ab", [False, True], ids=["single", "ab"])
@pytest.mark.parametrize("interactive", [False, True], ids=["noninteractive", "tty"])
def test_sudo_reuses_session_authentication(
    tmp_path: Path, sudo_environment: dict[str, str], ab: bool, interactive: bool
) -> None:
    cache = Path(sudo_environment["MEMORY_TEST_SUDO_CACHE"])
    if not interactive:
        cache.write_text(str(os.getsid(0)))
    binary = executable(tmp_path / "observer", FAKE_TRACE.replace("MEMORY_MIB", "1"))
    output = tmp_path / "result"
    args = command(output, binary)
    args.remove("--no-sudo")
    if ab:
        args.extend(["--ab", "--baseline-bin", str(binary)])
    result = run(args, sudo_environment, interactive=interactive)
    assert result.returncode == 0, result.stdout + result.stderr
    summary = json.loads((output / "resource-summary.json").read_text())
    assert summary["valid"]
    assert cache.exists()
    calls = [json.loads(line) for line in cache.with_suffix(".commands").read_text().splitlines()]
    assert (["-v"] in calls) == interactive
    assert all(measured["capture"] == "PASS" for measured in summary["runs"].values())
    assert_reaped(output)


def test_noninteractive_sudo_without_credentials_fails_fast(
    tmp_path: Path, sudo_environment: dict[str, str]
) -> None:
    binary = executable(tmp_path / "observer", FAKE_TRACE.replace("MEMORY_MIB", "1"))
    output = tmp_path / "result"
    args = command(output, binary)
    args.remove("--no-sudo")
    result = run(args, sudo_environment)
    assert result.returncode == 1, result.stdout + result.stderr
    cache = Path(sudo_environment["MEMORY_TEST_SUDO_CACHE"])
    assert not cache.exists()
    calls = [json.loads(line) for line in cache.with_suffix(".commands").read_text().splitlines()]
    assert ["-v"] not in calls
    assert "sudo: a password is required" in (output / "run/worker.log").read_text()
    assert_reaped(output)


def test_sudo_launcher_cancels_worker_without_fresh_credentials(
    tmp_path: Path, sudo_environment: dict[str, str]
) -> None:
    Path(sudo_environment["MEMORY_TEST_SUDO_CACHE"]).write_text(str(os.getsid(0)))
    binary = executable(tmp_path / "observer", FAKE_TRACE.replace("MEMORY_MIB", "1"))
    output = tmp_path / "result"
    args = command(output, binary) + ["--duration", "30"]
    args.remove("--no-sudo")
    process = subprocess.Popen(
        args,
        env=sudo_environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 15
        while not (output / "run/agents/start.json").exists():
            if process.poll() is not None or time.monotonic() >= deadline:
                pytest.fail("sudo worker did not start: " + str(process.communicate(timeout=2)))
            time.sleep(0.05)
        Path(sudo_environment["MEMORY_TEST_SUDO_CACHE"]).unlink()
        process.send_signal(signal.SIGINT)
        stdout, stderr = process.communicate(timeout=20)
        assert process.returncode == 130, stdout + stderr
        assert not json.loads((output / "resource-summary.json").read_text())["valid"]
        assert_reaped(output)
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()


def test_load_runner_uses_selected_python(
    tmp_path: Path, environment: dict[str, str], isolated_runtime: Path
) -> None:
    tools = tmp_path / "tools"
    tools.mkdir()
    executable(tools / "python3", "#!/bin/sh\necho unexpected Python interpreter >&2\nexit 90\n")
    output = tmp_path / "result"
    result = run(
        [
            "bash",
            str(SINGLE_RUN / "run.sh"),
            "--agents",
            "2",
            "--tokens-per-second",
            "100",
            "--tokens-per-response",
            "100",
            "--duration",
            "2",
            "--startup-delay",
            "0",
            "--rate-tolerance",
            "0.15",
            "--port",
            str(free_port()),
            "--output-dir",
            str(output),
        ],
        dict(
            environment,
            PYTHON=str(isolated_runtime / "bin/python3"),
            PATH=f"{tools}:{environment['PATH']}",
            PYTHONNOUSERSITE="1",
        ),
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert json.loads((output / "agent-summary.json").read_text())["passed"]


def fixture_repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    component = repo / "src/agentsight"
    component.mkdir(parents=True)
    shutil.copytree(
        SINGLE_RUN,
        component / "scripts/benchmark/single_run",
        ignore=shutil.ignore_patterns("__pycache__"),
    )
    shutil.copy2(SINGLE_RUN.parents[2] / "agentsight.json", component / "agentsight.json")
    (component / "Cargo.toml").write_text("# Fake builder fixture\n")
    executable(component / "observer", FAKE_TRACE.replace("MEMORY_MIB", "32"))
    (repo / "deleted").write_text("before")
    (repo / "staged").write_text("before")
    git(repo, "init", "-q")
    git(repo, "config", "user.name", "Benchmark Test")
    git(repo, "config", "user.email", "benchmark@example.invalid")
    git(repo, "add", ".")
    git(repo, "commit", "-qm", "fixture baseline")
    (repo / "staged").write_text("staged")
    git(repo, "add", "staged")
    (repo / "staged").write_text("staged and unstaged")
    (repo / "deleted").unlink()
    (repo / "untracked").write_text("untracked")
    executable(component / "observer", FAKE_TRACE.replace("MEMORY_MIB", "4"))
    return repo


def test_git_ab_freezes_dirty_tree_and_preserves_index(
    tmp_path: Path, environment: dict[str, str]
) -> None:
    repo = fixture_repo(tmp_path)
    tools = tmp_path / "tools"
    tools.mkdir()
    executable(
        tools / "cargo",
        """#!/usr/bin/env python3
import os, shutil
from pathlib import Path
destination = Path(os.environ["CARGO_TARGET_DIR"]) / "release/agentsight"
destination.parent.mkdir(parents=True, exist_ok=True)
shutil.copy2("observer", destination)
assert Path("../../staged").read_text() in ("before", "staged and unstaged")
""",
    )
    index = Path(git(repo, "rev-parse", "--absolute-git-dir")) / "index"
    status_before, head_before = git(repo, "status", "--porcelain"), git(repo, "rev-parse", "HEAD")
    index_before = index.read_bytes()
    output = tmp_path / "result"
    script = repo / "src/agentsight/scripts/benchmark/single_run/resource_run.sh"
    args = command(output, tmp_path / "unused", script)
    start = args.index("--agentsight-bin")
    del args[start : start + 2]
    result = run(
        # Let the sampler initialize before the short measured load window.
        args + ["--ab", "--baseline", "HEAD", "--startup-delay", "1"],
        dict(environment, PATH=f"{tools}:{environment['PATH']}"),
    )
    assert result.returncode == 0, result.stdout + result.stderr
    summary = json.loads((output / "resource-summary.json").read_text())
    assert summary["valid"]
    assert summary["runs"]["before"]["source"]["commit"] == head_before
    assert summary["runs"]["after"]["source"]["dirty"]
    assert summary["runs"]["before"]["source"]["tree"] != summary["runs"]["after"]["source"]["tree"]
    assert git(repo, "rev-parse", "HEAD") == head_before
    assert git(repo, "status", "--porcelain") == status_before
    assert index.read_bytes() == index_before
    assert git(repo, "worktree", "list", "--porcelain").count("worktree ") == 1
    assert_reaped(output)


def test_snapshot_contains_deletions_and_untracked_without_staging(tmp_path: Path) -> None:
    repo = fixture_repo(tmp_path)
    output = tmp_path / "output"
    output.mkdir()
    inputs = resource_inputs.Inputs(repo, output)
    try:
        source = inputs.prepare("after", "working-tree", None)
        snapshot = Path(source["component"]).parents[1]
        assert not (snapshot / "deleted").exists()
        assert (snapshot / "untracked").read_text() == "untracked"
        assert (snapshot / "staged").read_text() == "staged and unstaged"
    finally:
        inputs.close()


@pytest.mark.parametrize(
    ("output_name", "neighbor_name"),
    [("run[1]", "run1"), ("nested/run[1]", "nested/run1"), ("run", "run-other")],
)
def test_snapshot_excludes_only_the_literal_output_directory(
    tmp_path: Path, output_name: str, neighbor_name: str
) -> None:
    repo = fixture_repo(tmp_path)
    neighbor = repo / neighbor_name
    neighbor.mkdir(parents=True)
    (neighbor / "modified").write_text("before")
    (neighbor / "deleted").write_text("before")
    git(repo, "add", "--", neighbor_name)
    git(repo, "commit", "-qm", "add snapshot neighbor")
    (neighbor / "modified").write_text("staged")
    git(repo, "add", "--", neighbor_name)
    (neighbor / "modified").write_text("staged and unstaged")
    (neighbor / "deleted").unlink()
    (neighbor / "untracked").write_text("untracked")
    output = repo / output_name
    (output / "logs/nested").mkdir(parents=True)
    (output / "settings.json").write_text("{}")
    (output / "logs/nested/run.log").write_text("generated output")
    head_before = git(repo, "rev-parse", "HEAD")
    status_before = git(repo, "status", "--porcelain")
    index = Path(git(repo, "rev-parse", "--absolute-git-dir")) / "index"
    index_before = index.read_bytes()
    inputs = resource_inputs.Inputs(repo, output)
    try:
        source = inputs.prepare("after", "working-tree", None)
        snapshot = Path(source["component"]).parents[1]
        assert (snapshot / neighbor_name / "modified").read_text() == "staged and unstaged"
        assert not (snapshot / neighbor_name / "deleted").exists()
        assert (snapshot / neighbor_name / "untracked").read_text() == "untracked"
        assert not (snapshot / output_name).exists()
        assert source["dirty"]
        assert index.read_bytes() == index_before
    finally:
        inputs.close()
    assert git(repo, "rev-parse", "HEAD") == head_before
    assert git(repo, "status", "--porcelain") == status_before
    assert (neighbor / "modified").read_text() == "staged and unstaged"
    assert (output / "logs/nested/run.log").read_text() == "generated output"
    assert git(repo, "worktree", "list", "--porcelain").count("worktree ") == 1


def test_build_failure_removes_worktrees_and_reports_reason(
    tmp_path: Path, environment: dict[str, str]
) -> None:
    repo = fixture_repo(tmp_path)
    tools = tmp_path / "tools"
    tools.mkdir()
    executable(tools / "cargo", "#!/bin/sh\nexit 42\n")
    output = tmp_path / "result"
    script = repo / "src/agentsight/scripts/benchmark/single_run/resource_run.sh"
    args = command(output, tmp_path / "unused", script)
    start = args.index("--agentsight-bin")
    del args[start : start + 2]
    result = run(
        args + ["--ab", "--baseline", "HEAD"],
        dict(environment, PATH=f"{tools}:{environment['PATH']}"),
    )
    assert result.returncode == 1 and "release build failed" in result.stdout
    assert not json.loads((output / "resource-summary.json").read_text())["valid"]
    assert git(repo, "worktree", "list", "--porcelain").count("worktree ") == 1
    assert not list(output.glob("*/worker-pids.json"))


@pytest.mark.parametrize(
    ("state", "message"),
    [
        ("missing", "AgentSight binary does not exist"),
        ("directory", "AgentSight binary is not a regular file"),
        ("non-executable", "AgentSight binary is not executable"),
    ],
)
def test_invalid_binary_reports_specific_cause(
    tmp_path: Path, environment: dict[str, str], state: str, message: str
) -> None:
    binary = tmp_path / "observer"
    if state == "directory":
        binary.mkdir()
    elif state == "non-executable":
        binary.write_text("#!/bin/sh\nexit 0\n")
        binary.chmod(0o644)
    output = tmp_path / "result"
    result = run(command(output, binary), environment)
    assert result.returncode == 1, result.stdout + result.stderr
    assert f"{message}: {binary}" in result.stdout
    assert not json.loads((output / "resource-summary.json").read_text())["valid"]
    assert not list(output.glob("*/worker-pids.json"))


@pytest.mark.parametrize(
    "options",
    [
        ["--ab"],
        ["--agents", "0"],
        ["--duration", "nan"],
        ["--rate-tolerance", "1"],
        ["--connection-capacity", "0"],
        ["--ring-buffer-mb", "0"],
        ["--ring-buffer-mb", "3"],
        ["--cpu-budget-percent", "-1"],
        ["--cpu-budget-percent", "101"],
        ["--candidate-lib", "missing.so"],
        ["--ffi-api", "v2"],
        ["--ffi", "--agentsight-bin", "observer"],
        ["--ffi", "--baseline-lib", "missing.so"],
    ],
)
def test_invalid_arguments_fail_before_build(options: list[str]) -> None:
    result = subprocess.run(
        ["bash", str(SINGLE_RUN / "resource_run.sh"), *options], capture_output=True, text=True
    )
    assert result.returncode == 2 and "error:" in result.stderr


def test_reduction_reports_regressions_and_zero_baselines() -> None:
    assert resource_report.reduction(100, 125) == {"mib": -25, "percent": -25}
    assert resource_report.reduction(0, 10) == {"mib": -10, "percent": None}
