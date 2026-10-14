"""Exercise real HTTPS streams, token-rate verification and worker cleanup."""

from __future__ import annotations

import json
import ssl
import subprocess
import sys
import threading
import time
from collections.abc import Iterator
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

SINGLE_RUN = Path(__file__).parents[1] / "single_run"
sys.path.insert(0, str(SINGLE_RUN))
import mock_llm_server
import validate_results


@pytest.fixture
def https_server(tmp_path: Path) -> Iterator[mock_llm_server.BenchmarkHTTPServer]:
    certificate_command = (
        "openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj /CN=localhost".split()
    )
    subprocess.run(
        certificate_command + ["-keyout", str(tmp_path / "key"), "-out", str(tmp_path / "cert")],
        check=True,
        capture_output=True,
    )
    server = mock_llm_server.BenchmarkHTTPServer(("127.0.0.1", 0), mock_llm_server.BenchmarkHandler)
    server.settings = SimpleNamespace(
        sse=True,
        chunks=1,
        chunk_bytes=2,
        chunk_delay=0,
        tokens_per_second=100,
        output_tokens=100,
        request_recorder=mock_llm_server.RequestRecorder(tmp_path / "requests"),
    )
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(str(tmp_path / "cert"), str(tmp_path / "key"))
    server.socket = context.wrap_socket(server.socket, server_side=True)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        server.settings.request_recorder.close()


def cli(script: Path, options: dict[str, Any]) -> list[str]:
    args = [sys.executable, str(script)]
    for flag, value in options.items():
        args.extend((flag, str(value)))
    return args


def command(
    server: mock_llm_server.BenchmarkHTTPServer, output: Path, agents: int = 2, rate: int = 100
) -> list[str]:
    return cli(
        SINGLE_RUN / "load/agents.py",
        {
            "--agents": agents,
            "--tokens-per-second": rate,
            "--tokens-per-response": server.settings.output_tokens,
            "--duration": 2,
            "--startup-delay": 0,
            "--rate-tolerance": 0.15,
            "--base-url": f"https://127.0.0.1:{server.server_port}",
            "--output-dir": output / "agents",
            "--run-id": "test",
        },
    )


@pytest.mark.parametrize("agent_count,rate", [(2, 50), (3, 100)])
def test_independent_clients_receive_the_requested_rate(
    https_server: mock_llm_server.BenchmarkHTTPServer,
    tmp_path: Path,
    agent_count: int,
    rate: int,
) -> None:
    https_server.settings.tokens_per_second = rate
    result = subprocess.run(
        command(https_server, tmp_path, agent_count, rate),
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    summary = json.loads((tmp_path / "agent-summary.json").read_text())
    assert summary["passed"]
    assert summary["distinct_agent_pids"] == agent_count
    assert summary["peak_active_streams"] == agent_count
    assert all(abs(agent["tokens_per_second"] / rate - 1) < 0.15 for agent in summary["agents"])
    expected, successful = validate_results.load_expected(tmp_path / "agents.jsonl.gz")
    accepted, _ = validate_results.load_request_log(tmp_path / "requests/test.jsonl")
    assert expected == successful == accepted
    assert len(expected) >= agent_count
    if rate == 100:
        assert len(expected) >= 2 * agent_count
    report = tmp_path / "benchmark-report.md"
    subprocess.run(
        cli(
            SINGLE_RUN / "render_report.py",
            {
                "--output": report,
                "--protocol": "sse",
                "--qps": 2,
                "--duration": 2,
                "--load-results": tmp_path / "agents.jsonl.gz",
                "--agent-summary": tmp_path / "agent-summary.json",
            },
        ),
        check=True,
        capture_output=True,
    )
    assert "Load verification: PASS" in report.read_text()


def test_usage_mismatch_fails_instead_of_reporting_success(
    https_server: mock_llm_server.BenchmarkHTTPServer,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    original = mock_llm_server.token_response_events

    def wrong_usage(request_id: str, output_tokens: int) -> Iterator[dict[str, Any]]:
        for event in original(request_id, output_tokens):
            if "usage" in event:
                event["usage"]["completion_tokens"] += 1
            yield event

    monkeypatch.setattr(mock_llm_server, "token_response_events", wrong_usage)
    result = subprocess.run(command(https_server, tmp_path), capture_output=True, timeout=20)
    assert result.returncode == 1
    summary = json.loads((tmp_path / "agent-summary.json").read_text())
    assert not summary["passed"]
    assert len(summary["failed_agents"]) == 2
    assert all("usage mismatch" in agent["error"] for agent in summary["agents"])


@pytest.mark.parametrize(
    "option,value",
    [
        ("--agents", "0"),
        ("--tokens-per-second", "nan"),
        ("--tokens-per-second", "0"),
        ("--duration", "-1"),
    ],
)
def test_invalid_load_parameters_are_rejected(tmp_path: Path, option: str, value: str) -> None:
    result = subprocess.run(
        cli(SINGLE_RUN / "load/agents.py", {"--output-dir": tmp_path / "unused", option: value})
        + ["--check-only"],
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert result.returncode == 2
    assert "finite and positive" in result.stderr
    assert not (tmp_path / "unused").exists()


def test_interrupt_reaps_all_client_processes(
    https_server: mock_llm_server.BenchmarkHTTPServer, tmp_path: Path
) -> None:
    https_server.settings.output_tokens = 1000
    args = command(https_server, tmp_path)
    args[args.index("--duration") + 1] = "30"
    process = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        deadline = time.monotonic() + 10
        while not (tmp_path / "agents/start.json").exists():
            assert process.poll() is None
            assert time.monotonic() < deadline
            time.sleep(0.05)
        pids = [
            json.loads(path.read_text())["pid"]
            for path in (tmp_path / "agents").glob("ready-*.json")
        ]
        assert len(pids) == 2
        process.terminate()
        process.communicate(timeout=6)
        assert process.returncode == 130
        assert all(not Path(f"/proc/{pid}").exists() for pid in pids)
    finally:
        if process.poll() is None:
            process.kill()
            process.communicate()


@pytest.mark.parametrize("interrupt", [False, True], ids=["capture-release", "interrupt-drain"])
def test_clients_remain_alive_until_capture_releases_them(
    https_server: mock_llm_server.BenchmarkHTTPServer, tmp_path: Path, interrupt: bool
) -> None:
    marker = tmp_path / "load-complete"
    process = subprocess.Popen(
        command(https_server, tmp_path) + ["--capture-marker", str(marker)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        deadline = time.monotonic() + 10
        while not marker.exists():
            assert process.poll() is None
            assert time.monotonic() < deadline
            time.sleep(0.05)
        summary = json.loads((tmp_path / "agent-summary.json").read_text())
        assert summary["passed"] and summary["duration_seconds"] == 2
        pids = [agent["pid"] for agent in summary["agents"]]
        time.sleep(0.2)
        assert process.poll() is None
        assert all(
            Path(f"/proc/{pid}/status").exists()
            and "State:\tZ" not in Path(f"/proc/{pid}/status").read_text()
            for pid in pids
        )
        if interrupt:
            process.terminate()
        else:
            marker.with_name(marker.name + ".release").touch()
        stdout, stderr = process.communicate(timeout=6)
        assert process.returncode == (130 if interrupt else 0), stdout + stderr
        assert all(not Path(f"/proc/{pid}").exists() for pid in pids)
    finally:
        if process.poll() is None:
            process.kill()
            process.communicate()
