from __future__ import annotations

import gzip
import json
import os
import socket
import sys
import threading
from collections.abc import Callable
from copy import deepcopy
from pathlib import Path
from types import SimpleNamespace

import pytest

BENCHMARK_DIR = Path(__file__).parents[1]
sys.path.insert(0, str(BENCHMARK_DIR / "campaign"))
sys.path.insert(0, str(BENCHMARK_DIR / "single_run"))

import campaign
import campaign_evidence
import h2load_stats
import mock_llm_server
import render_report
import validate_results


def summary(qps: float = 2) -> dict[str, object]:
    return {
        "input_qps": qps,
        "effective_qps": qps,
        "http_success_rate": 1.0,
        "http_error_rate": 0.0,
        "timeout_rate": 0.0,
        "trace_completeness": 1.0,
        "token_accuracy": 1.0,
        "drop_rate": 0.0,
        "latency_ms": {"p99": 10.0},
        "resources": {
            "rss_mb": {"avg": 100.0, "max": 110.0},
            "channel_length": {"avg": 2.0, "max": 3.0},
            "connection_cache_bytes": {"avg": 20.0, "max": 30.0},
            "event_channel_bytes": {"avg": 40.0, "max": 50.0},
            "pending_genai_bytes": {"avg": 60.0, "max": 70.0},
        },
        "process_survived": True,
        "runtime_clean": True,
    }


def thresholds() -> dict[str, float]:
    return {
        "min_throughput_ratio": 0.9,
        "min_http_success_rate": 0.99,
        "min_trace_completeness": 0.999,
        "min_token_accuracy": 1.0,
        "max_p99_ms": 100,
        "max_drop_rate": 0.01,
        "max_rss_mb": 200,
        "max_rss_slope_mb_per_hour": 10,
        "max_fd_slope_per_hour": 1,
        "max_thread_slope_per_hour": 1,
        "max_socket_slope_per_hour": 1,
        "max_recovery_seconds": 10,
    }


def test_h2load_summary_preserves_protocol_specific_measurements(
    tmp_path: Path,
) -> None:
    output = tmp_path / "h2load.txt"
    output.write_text(
        "finished in 1.10s, 90.83 req/s, 10.65KB/s\n"
        "requests: 100 total, 100 started, 100 done, 98 succeeded, 2 failed, "
        "1 errored, 1 timeout\n"
        "time for request: 120us 1.20s 12.50ms 2.00ms\n",
        encoding="utf-8",
    )
    load = render_report.summarize_h2load(output)
    assert load["requests"] == 100
    assert load["http_success"] == 98
    assert load["timeouts"] == 1
    assert load["throughput"] == 90.83
    assert load["latency"]["min"] == 0.12
    assert load["latency"]["avg"] == 12.5
    assert load["latency"]["max"] == 1200
    assert h2load_stats.duration_ms("bad") is None


def test_http2_mock_serves_health_sse_json_and_not_found() -> None:
    pytest.importorskip("h2")
    from h2.config import H2Configuration
    from h2.connection import H2Connection
    from h2.events import DataReceived, ResponseReceived, StreamEnded

    server_socket, client_socket = socket.socketpair()
    client_socket.settimeout(2)
    settings = SimpleNamespace(chunks=1, chunk_bytes=4, chunk_delay=0, sse=True)
    thread = threading.Thread(
        target=mock_llm_server.serve_h2,
        args=(server_socket, settings, False),
    )
    thread.start()
    connection = H2Connection(config=H2Configuration(client_side=True))
    connection.initiate_connection()
    client_socket.sendall(connection.data_to_send())

    def request(stream_id: int, path: str, body: bytes = b"") -> tuple[str, bytes]:
        headers = [
            (":method", "POST" if body else "GET"),
            (":scheme", "https"),
            (":authority", "localhost"),
            (":path", path),
        ]
        connection.send_headers(stream_id, headers, end_stream=not body)
        if body:
            connection.send_data(stream_id, body, end_stream=True)
        client_socket.sendall(connection.data_to_send())
        status = ""
        payload = bytearray()
        while True:
            events = connection.receive_data(client_socket.recv(65535))
            ended = False
            for event in events:
                if isinstance(event, ResponseReceived) and event.stream_id == stream_id:
                    status = dict(event.headers)[b":status"].decode()
                elif isinstance(event, DataReceived) and event.stream_id == stream_id:
                    payload.extend(event.data)
                    connection.acknowledge_received_data(
                        event.flow_controlled_length, stream_id
                    )
                elif isinstance(event, StreamEnded) and event.stream_id == stream_id:
                    ended = True
            pending = connection.data_to_send()
            if pending:
                client_socket.sendall(pending)
            if ended:
                return status, bytes(payload)

    try:
        assert request(1, "/healthz") == ("200", b"OK")
        status, body = request(
            3,
            "/v1/chat/completions",
            b'{"request_id":"bench-h2","stream":true}',
        )
        assert status == "200"
        assert b"bench-h2" in body and b"[DONE]" in body
        settings.sse = False
        status, body = request(5, "/v1/chat/completions", b"{}")
        assert status == "200"
        assert json.loads(body)["request_id"] == "bench-missing"
        assert request(7, "/missing")[0] == "404"
    finally:
        client_socket.close()
        thread.join(timeout=5)
        server_socket.close()
    assert not thread.is_alive()


def test_runtime_log_capture_detects_fatal_patterns_and_rotation(
    tmp_path: Path,
) -> None:
    source = tmp_path / "agentsight.log"
    destination = tmp_path / "captured.log"
    source.write_text(
        "old line that is longer than the rotated file\n", encoding="utf-8"
    )
    start = campaign.log_position(source)
    with source.open("a", encoding="utf-8") as handle:
        handle.write("worker panicked at src/lib.rs\n")
    clean, errors = campaign.capture_runtime_log(source, start, destination)
    assert clean is False
    assert errors == ["panic"]
    assert destination.read_text(encoding="utf-8").startswith("worker panicked")

    source.write_text("new file after rotation\n", encoding="utf-8")
    clean, errors = campaign.capture_runtime_log(source, start, destination)
    assert clean is True
    assert errors == []
    assert campaign.log_position(tmp_path / "missing") is None
    assert campaign.capture_runtime_log(None, None, destination) == (None, [])


def test_campaign_validation_rejects_unsafe_formal_inputs(tmp_path: Path) -> None:
    from test_benchmark import campaign_data

    base = campaign_data(tmp_path)

    def invalid(mutator: Callable[[dict[str, object]], None], message: str) -> None:
        value = deepcopy(base)
        mutator(value)
        with pytest.raises((TypeError, ValueError), match=message):
            campaign.validate_campaign(value)

    invalid(lambda value: value.update(schema_version=2), "schema_version")
    invalid(
        lambda value: value.update(comparison_mode="invalid"),
        "comparison_mode",
    )
    invalid(lambda value: value.update(versions={}), "exactly baseline")
    invalid(
        lambda value: value["versions"].update(baseline="bad"),
        "baseline must be an object",
    )
    invalid(
        lambda value: value["versions"]["baseline"].update(log_file=""),
        "log_file must be",
    )
    invalid(lambda value: value.update(thresholds=[]), "thresholds must be")
    invalid(
        lambda value: value["thresholds"].update(max_rss_mb=float("nan")),
        "finite non-negative",
    )
    invalid(
        lambda value: value["thresholds"].update(max_drop_rate=2),
        "ratio between",
    )
    invalid(
        lambda value: value["capacity"].update(qps_start=0),
        "qps_start",
    )
    invalid(
        lambda value: value["capacity"].update(qps_safety_max=50),
        "safety_max",
    )
    invalid(
        lambda value: value["capacity"].update(qps_resolution=500),
        "at least qps_resolution",
    )
    invalid(
        lambda value: value["capacity"].update(pretest_duration_seconds=0),
        "pretest_duration_seconds",
    )
    invalid(
        lambda value: value["capacity"].update(pretest_warmup_seconds=-1),
        "pretest_warmup_seconds",
    )
    invalid(
        lambda value: value["capacity"].update(search_start_ratio=1),
        "search_start_ratio",
    )
    invalid(
        lambda value: value["matrix"].update(qps=[-1, 2, 3, 4, 5]),
        "positive integers",
    )
    invalid(lambda value: value.update(smoke=[]), "smoke must be")
    invalid(lambda value: value.update(safety=[]), "safety must be")
    invalid(
        lambda value: value.update(safety={"max_results_gb": 0}),
        "safety.max_results_gb",
    )
    invalid(
        lambda value: value.update(safety={"max_agentsight_rss_mb": 50}),
        "must be at least thresholds.max_rss_mb",
    )
    invalid(
        lambda value: value["matrix"].update(duration_seconds=0),
        "duration_seconds",
    )
    invalid(
        lambda value: value["soak"].update(warmup_seconds=-1),
        "warmup_seconds",
    )
    invalid(lambda value: value["load"].update(protocol="h2"), "sse or json")
    invalid(lambda value: value["load"].update(payload_kb=0), "payload_kb")
    invalid(
        lambda value: value["recovery"].update(tolerance_ratio=1),
        "tolerance_ratio",
    )
    invalid(
        lambda value: value["recovery"].update(
            recovery_window_seconds=value["recovery"]["recover_seconds"] + 1
        ),
        "cannot exceed",
    )


def test_capacity_requires_a_confirmed_failure_boundary(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = {
        "capacity": {
            "qps_start": 100,
            "qps_resolution": 50,
            "qps_safety_max": 200,
            "pretest_warmup_seconds": 0,
            "pretest_duration_seconds": 1,
            "search_start_ratio": 0.8,
            "probe_warmup_seconds": 0,
            "probe_duration_seconds": 1,
            "confirm_warmup_seconds": 0,
            "confirm_duration_seconds": 1,
            "confirm_repetitions": 3,
        }
    }

    def always_pass(*_: object, **__: object) -> dict[str, object]:
        return {"evaluation": {"verdict": "PASS"}}

    monkeypatch.setattr(campaign, "run_once", always_pass)
    result = campaign.capacity(data, tmp_path, "baseline", os.getpid())
    assert result["maximum_sustainable_qps"] is None
    assert result["confirmed_lower_bound_qps"] == 200
    assert result["safety_limit_reached"] is True
    assert result["boundary_confirmed"] is False


def test_resolved_overload_uses_the_lower_capacity_version(tmp_path: Path) -> None:
    data = {
        "versions": {"baseline": {}, "optimized": {}},
        "capacity": {"qps_resolution": 50},
        "matrix": {"qps": []},
    }
    (tmp_path / "capacity-baseline.json").write_text(
        json.dumps({"maximum_sustainable_qps": 1000, "first_failed_qps": 1050}),
        encoding="utf-8",
    )
    (tmp_path / "capacity-optimized.json").write_text(
        json.dumps({"maximum_sustainable_qps": 800, "first_failed_qps": 900}),
        encoding="utf-8",
    )
    matrix, soak, overload = campaign.resolved_qps(data, tmp_path)
    assert matrix == [150, 300, 450, 600, 800]
    assert soak == 600
    assert overload == 900
    assert (
        json.loads((tmp_path / "campaign-resolution.json").read_text())[
            "common_max_qps"
        ]
        == 800
    )


def test_validation_waits_for_asynchronous_capture(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    reports = [
        {},
        {"bench-1": [("complete", 20)]},
    ]
    monkeypatch.setattr(
        validate_results,
        "load_captured",
        lambda *_: reports.pop(0) if len(reports) > 1 else reports[0],
    )
    monkeypatch.setattr(validate_results.time, "sleep", lambda _: None)
    report = validate_results.wait_for_report(
        tmp_path / "events.db",
        "bench-",
        {"bench-1"},
        {"bench-1"},
        20,
        0.999,
        1,
        0.01,
    )
    assert report["completeness_ratio"] == 1


def write_recovery_artifacts(run_path: Path) -> None:
    measurement = run_path.parent / "measurement"
    measurement.mkdir(parents=True)
    measurement.joinpath("metrics.csv").write_text(
        "timestamp,rss_mb,channel_length,connection_cache_bytes\n"
        "0,130,5,50\n1,105,2,20\n2,104,2,20\n3,103,2,20\n",
        encoding="utf-8",
    )
    rows = []
    for second in range(4):
        request_count = 1 if second == 0 else 2
        for _ in range(request_count):
            rows.append(
                {
                    "metric": "benchmark_requests",
                    "data": {"time": str(second), "value": 1},
                }
            )
        rows.append(
            {
                "metric": "benchmark_latency",
                "data": {"time": str(second), "value": 20 if second == 0 else 10},
            }
        )
    with gzip.open(measurement / "k6.jsonl.gz", "wt", encoding="utf-8") as handle:
        handle.write("\n".join(json.dumps(row) for row in rows) + "\n")


def test_recovery_and_fault_evidence_apply_every_gate(tmp_path: Path) -> None:
    phases = {}
    for label in campaign_evidence.RECOVERY_PHASES:
        run_path = tmp_path / label / "run-result.json"
        run = {
            "summary": summary(),
            "evaluation": {"verdict": "FAIL" if label == "overload" else "PASS"},
        }
        phases[label] = (run_path, run)
        if label == "recover":
            write_recovery_artifacts(run_path)
    recovery = campaign_evidence.recovery_outcome(
        phases,
        {"tolerance_ratio": 0.1, "recovery_window_seconds": 2},
        thresholds(),
    )
    assert recovery["verdict"] == "PASS"
    assert set(recovery["seconds"].values()) == {1.0}

    run_path = tmp_path / "fault" / "run-result.json"
    measurement = run_path.parent / "measurement"
    measurement.mkdir(parents=True)
    measurement.joinpath("fault-results.json").write_text(
        json.dumps(
            {
                "outcomes": {
                    name: {"handled": 1} for name in campaign_evidence.FAULT_CASES
                },
                "server_healthy_after": True,
                "process_alive_before": True,
                "process_alive_after": True,
            }
        ),
        encoding="utf-8",
    )
    fault = campaign_evidence.fault_outcome(
        run_path,
        {"summary": summary()},
        {"repetitions_per_case": 1},
        thresholds(),
    )
    assert fault == {"verdict": "PASS", "missing": [], "failed": []}

    wrong_token = summary()
    wrong_token["token_accuracy"] = 0.99
    fault = campaign_evidence.fault_outcome(
        run_path,
        {"summary": wrong_token},
        {"repetitions_per_case": 1},
        thresholds(),
    )
    assert fault["verdict"] == "FAIL"
    assert fault["failed"] == ["token_accuracy"]


def test_campaign_audit_rejects_partial_evidence() -> None:
    issues = campaign_evidence.audit_campaign(
        {
            "capacity": {"qps_resolution": 50, "confirm_repetitions": 3},
            "matrix": {"qps": [], "repetitions": 3},
            "soak": {"duration_seconds": 14400, "warmup_seconds": 600},
            "recovery": {
                "repetitions": 3,
                "stable_seconds": 600,
                "overload_seconds": 300,
                "recover_seconds": 900,
            },
            "fault": {"duration_seconds": 300, "warmup_seconds": 180},
        },
        [],
        {"baseline": {}, "optimized": {}},
        {},
        {},
        {},
    )
    assert "five unique common matrix QPS levels are unavailable" in issues
    assert "full Rust regression gates were not recorded" in issues


def test_campaign_audit_accepts_only_complete_formal_evidence() -> None:
    campaign_data = {
        "capacity": {"qps_resolution": 50, "confirm_repetitions": 3},
        "matrix": {
            "qps": [100, 200, 300, 400, 500],
            "repetitions": 3,
            "warmup_seconds": 180,
            "duration_seconds": 900,
        },
        "soak": {"warmup_seconds": 600, "duration_seconds": 14400},
        "recovery": {
            "repetitions": 3,
            "stable_seconds": 600,
            "overload_seconds": 300,
            "recover_seconds": 900,
        },
        "fault": {"duration_seconds": 300, "warmup_seconds": 180},
    }
    capacities = {
        version: {
            "maximum_sustainable_qps": 500,
            "first_failed_qps": 550,
            "safety_limit_reached": False,
            "boundary_confirmed": True,
            "confirmation": {
                "500": ["PASS", "PASS", "PASS"],
                "550": ["FAIL", "FAIL", "FAIL"],
            },
        }
        for version in campaign_evidence.VERSIONS
    }
    items = []
    for version in campaign_evidence.VERSIONS:
        for qps in campaign_data["matrix"]["qps"]:
            for repetition in range(1, 4):
                items.append(
                    (
                        Path(f"/{version}/{qps}/{repetition}/run-result.json"),
                        {
                            "scenario": "matrix",
                            "version": version,
                            "label": f"qps-{qps}",
                            "repetition": repetition,
                            "duration_seconds": 900,
                            "warmup_seconds": 180,
                            "evaluation": {"verdict": "PASS"},
                        },
                    )
                )
        items.append(
            (
                Path(f"/{version}/soak/run-result.json"),
                {
                    "scenario": "soak",
                    "version": version,
                    "qps": 400,
                    "label": "qps-400",
                    "duration_seconds": 14400,
                    "warmup_seconds": 600,
                    "evaluation": {"verdict": "PASS"},
                    "summary": summary(),
                },
            )
        )
        for repetition in range(1, 4):
            for label, qps, duration in (
                ("stable", 400, 600),
                ("overload", 550, 300),
                ("recover", 400, 900),
            ):
                items.append(
                    (
                        Path(
                            f"/{version}/recovery/{label}/{repetition}/run-result.json"
                        ),
                        {
                            "scenario": "recovery",
                            "version": version,
                            "label": label,
                            "repetition": repetition,
                            "qps": qps,
                            "duration_seconds": duration,
                            "warmup_seconds": 0,
                        },
                    )
                )
        items.append(
            (
                Path(f"/{version}/fault/run-result.json"),
                {
                    "scenario": "fault",
                    "version": version,
                    "qps": 400,
                    "duration_seconds": 300,
                    "warmup_seconds": 180,
                },
            )
        )
    recovery = {
        (version, repetition): {"verdict": "PASS"}
        for version in campaign_evidence.VERSIONS
        for repetition in range(1, 4)
    }
    faults = {version: {"verdict": "PASS"} for version in campaign_evidence.VERSIONS}
    regression = {
        "full": True,
        "checks": [
            {
                "command": command,
                "exit_code": 0,
            }
            for command in (
                "python -m diff_cover.diff_cover_tool coverage.xml --fail-under=85",
                "cargo fmt --all -- --check",
                "cargo clippy --workspace --all-targets -- -D warnings",
                "cargo test --workspace",
            )
        ],
    }
    assert (
        campaign_evidence.audit_campaign(
            campaign_data, items, capacities, recovery, faults, regression
        )
        == []
    )
