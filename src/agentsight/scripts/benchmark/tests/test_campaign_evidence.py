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
    assert clean is None  # Rotation lost part of the measured interval.
    assert errors == []
    assert campaign.log_position(tmp_path / "missing") is None
    assert campaign.capture_runtime_log(None, None, destination) == (None, [])


@pytest.mark.parametrize(
    "message",
    [
        "Failed to store GenAI event in batch flush: database is locked",
        "Failed to store analysis result: Failed to insert token record: database or disk is full",
        "Failed to insert pending call bench-1: database is locked",
        "Failed to insert deferred pending call bench-1: disk I/O error",
        "Failed to complete pending call: UNIQUE constraint failed: genai_events.call_id",
        "Failed to complete deferred pending call: disk I/O error",
        "[CrashDetect] Failed to persist pending call: disk I/O error",
        "[IdleDrain] Failed to persist pending call: disk I/O error",
        "[DrainCheck] FAIL persist: database is locked",
        "[DrainCheck] FAIL update session_id: database is locked",
        "Failed to store interruption event: database or disk is full",
        "Failed to store tool_failure interruption: database is locked",
        "Failed to persist Agent resource samples: disk I/O error",
        "[CrashDetect] Failed to record exit status for pid=123: database is locked",
        "[CrashDetect] Failed to clear stale exit status for pid=123: disk I/O error",
        "[CrashDetect] Failed to record agent_crash for pid=123: database is locked",
        "[DrainCheck] Failed to record OOM agent_crash for pid=123: disk I/O error",
        "[CrashDetect] Failed to mark pending interrupted for pid=123: database is locked",
        "Failed to mark pending calls as interrupted for pid=123: database is locked",
        "database insert failed: database is locked",
    ],
)
def test_runtime_log_storage_failures_reject_campaign(
    tmp_path: Path, message: str
) -> None:
    source = tmp_path / "agentsight.log"
    destination = tmp_path / "captured.log"
    source.write_text(message + "\n", encoding="utf-8")

    clean, errors = campaign.capture_runtime_log(source, 0, destination)
    assert clean is False
    assert errors == ["database_write"]
    assert destination.read_text(encoding="utf-8") == message + "\n"

    measured = summary()
    measured["runtime_clean"] = clean
    evaluation = campaign.evaluate(measured, thresholds())
    assert evaluation["verdict"] == "FAIL"
    assert evaluation["failed"] == ["runtime_clean"]


@pytest.mark.parametrize(
    "message",
    [
        "GenAISqliteStore initialized: db_size=0MB",
        "Analysis result saved",
        "database maintenance completed",
        "Failed to attach optional probe",
        "Failed to store GenAI events to JSONL: disk I/O error",
        "Database full (SQLITE_FULL), pruning old records (attempt 1/3)",
        "[DrainCheck] FAIL lookup session: database is locked",
    ],
)
def test_runtime_log_ignores_unrelated_storage_messages(
    tmp_path: Path, message: str
) -> None:
    source = tmp_path / "agentsight.log"
    source.write_text(message + "\n", encoding="utf-8")
    assert campaign.capture_runtime_log(source, 0, tmp_path / "captured.log") == (
        True,
        [],
    )


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


def test_campaign_validation_classifies_malformed_shapes(
    tmp_path: Path,
) -> None:
    """Malformed sections, QPS members, and thresholds get actionable errors."""
    from test_benchmark import campaign_data

    base = campaign_data(tmp_path)

    def invalid(mutator: Callable[[dict[str, object]], None], message: str) -> None:
        value = deepcopy(base)
        mutator(value)
        with pytest.raises((TypeError, ValueError), match=message):
            campaign.validate_campaign(value)

    invalid(lambda value: value.update(capacity=None), "capacity must be an object")
    invalid(
        lambda value: value.update(capacity=[{"qps_start": 100}]),
        "capacity must be an object",
    )
    invalid(lambda value: value.update(matrix="matrix"), "matrix must be an object")
    invalid(lambda value: value.update(matrix=None), "matrix must be an object")
    invalid(
        lambda value: value["matrix"].update(qps=[1, [2], 3, 4, 5]),
        "positive integers",
    )
    invalid(
        lambda value: value["matrix"].update(qps=[1, {"qps": 2}, 3, 4, 5]),
        "positive integers",
    )
    invalid(lambda value: value["matrix"].update(qps=5), "matrix.qps must be a list")
    invalid(
        lambda value: value["matrix"].update(qps="auto"), "matrix.qps must be a list"
    )
    invalid(
        lambda value: value["thresholds"].update(max_p99_ms=10**400),
        "finite non-negative",
    )
    invalid(
        lambda value: value["thresholds"].update(max_p99_ms=-(10**400)),
        "finite non-negative",
    )
    invalid(
        lambda value: value["capacity"].update(search_start_ratio=10**400),
        "search_start_ratio",
    )
    invalid(
        lambda value: value["recovery"].update(tolerance_ratio=False),
        "tolerance_ratio",
    )
    invalid(
        lambda value: value["recovery"].update(tolerance_ratio=True),
        "tolerance_ratio",
    )

    campaign.validate_campaign(base)
    empty_qps = deepcopy(base)
    empty_qps["matrix"]["qps"] = []
    campaign.validate_campaign(empty_qps)
    finite = deepcopy(base)
    finite["thresholds"]["max_p99_ms"] = 1e9
    campaign.validate_campaign(finite)
    ratio = deepcopy(base)
    ratio["thresholds"]["min_token_accuracy"] = 1.01
    with pytest.raises(ValueError, match="ratio between 0 and 1"):
        campaign.validate_campaign(ratio)


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


def test_recovery_evidence_rejects_malformed_references_and_summaries(
    tmp_path: Path,
) -> None:
    """A boolean or non-finite recovery reference is missing evidence, not a
    threshold; a phase artifact without a summary object is missing evidence,
    not a crash."""

    call = 0

    def phases_with(stable_summary: object) -> dict[str, object]:
        nonlocal call
        call += 1
        phases = {}
        for label in campaign_evidence.RECOVERY_PHASES:
            run_path = tmp_path / f"run{call}" / label / "run-result.json"
            run = {
                "summary": stable_summary if label == "stable" else summary(),
                "evaluation": {"verdict": "FAIL" if label == "overload" else "PASS"},
            }
            phases[label] = (run_path, run)
            if label == "recover":
                write_recovery_artifacts(run_path)
        return phases

    settings = {"tolerance_ratio": 0.1, "recovery_window_seconds": 2}

    # A boolean effective_qps reference is an int subclass: gating at
    # float(True) == 1.0 QPS lets every real sample count as recovered and
    # the run passes without any usable reference.
    boolean_reference = summary()
    boolean_reference["effective_qps"] = True
    outcome = campaign_evidence.recovery_outcome(
        phases_with(boolean_reference), settings, thresholds()
    )
    assert outcome["verdict"] == "INCONCLUSIVE"
    assert "effective_qps" in outcome["missing"]

    # A non-finite latency reference with lower-is-better compares every
    # sample against an infinite threshold — equally unusable.
    infinite_reference = summary()
    infinite_reference["latency_ms"] = {"p99": float("inf")}
    outcome = campaign_evidence.recovery_outcome(
        phases_with(infinite_reference), settings, thresholds()
    )
    assert outcome["verdict"] == "INCONCLUSIVE"
    assert "latency_p99_ms" in outcome["missing"]

    # A stable artifact whose summary is absent carries no evidence for any
    # specification; the gates must report missing instead of raising.
    phases = phases_with(summary())
    del phases["stable"][1]["summary"]  # type: ignore[index]
    outcome = campaign_evidence.recovery_outcome(phases, settings, thresholds())
    assert outcome["verdict"] == "INCONCLUSIVE"
    for name in ("effective_qps", "latency_p99_ms", "rss_mb"):
        assert name in outcome["missing"]

    # Control: with healthy references the same evidence passes.
    outcome = campaign_evidence.recovery_outcome(
        phases_with(summary()), settings, thresholds()
    )
    assert outcome["verdict"] == "PASS"


def test_fault_evidence_classifies_malformed_artifacts(tmp_path: Path) -> None:
    """Unusable fault artifact shapes and counters report missing evidence."""

    def evaluate(artifact: object, repetitions: int = 3) -> dict[str, object]:
        run_path = tmp_path / "fault" / "run-result.json"
        measurement = run_path.parent / "measurement"
        measurement.mkdir(parents=True, exist_ok=True)
        measurement.joinpath("fault-results.json").write_text(
            json.dumps(artifact), encoding="utf-8"
        )
        return campaign_evidence.fault_outcome(
            run_path,
            {"summary": summary()},
            {"repetitions_per_case": repetitions},
            thresholds(),
        )

    def complete(outcomes: object, **extra: object) -> dict[str, object]:
        return {
            "outcomes": outcomes,
            "server_healthy_after": True,
            "process_alive_before": True,
            "process_alive_after": True,
            **extra,
        }

    def each_case(entry: object) -> dict[str, object]:
        return {name: entry for name in campaign_evidence.FAULT_CASES}

    def full(entry: object) -> dict[str, object]:
        return complete(each_case(entry))

    # Non-object artifacts are unusable evidence, not crashes.
    assert evaluate(["not", "an", "object"]) == {
        "verdict": "INCONCLUSIVE",
        "missing": ["fault-results.json"],
        "failed": [],
    }
    assert evaluate(None) == {
        "verdict": "INCONCLUSIVE",
        "missing": ["fault-results.json"],
        "failed": [],
    }
    assert evaluate("artifact") == {
        "verdict": "INCONCLUSIVE",
        "missing": ["fault-results.json"],
        "failed": [],
    }
    # Unusable outcome containers leave every case missing.
    all_missing = {
        "verdict": "INCONCLUSIVE",
        "missing": [f"case:{name}" for name in sorted(campaign_evidence.FAULT_CASES)],
        "failed": [],
    }
    assert evaluate(complete(None)) == all_missing
    assert evaluate(complete(["invalid_json"])) == all_missing
    # Malformed per-case objects are missing evidence for that case only.
    scalar_case = full({"handled": 3})
    scalar_case["outcomes"]["invalid_json"] = 3
    assert evaluate(scalar_case)["missing"] == ["case:invalid_json"]
    list_case = full({"handled": 3})
    list_case["outcomes"]["invalid_json"] = ["handled", 3]
    assert evaluate(list_case)["missing"] == ["case:invalid_json"]
    string_case = full({"handled": 3})
    string_case["outcomes"]["invalid_json"] = "3"
    assert evaluate(string_case)["missing"] == ["case:invalid_json"]
    # Non-integer, boolean, and negative counters are missing, never satisfied.
    boolean_counter = full({"handled": 3})
    boolean_counter["outcomes"]["invalid_json"] = {"sent": True, "extra": 2}
    assert evaluate(boolean_counter)["missing"] == ["case:invalid_json"]
    boolean_only = full({"handled": 1})
    boolean_only["outcomes"]["invalid_json"] = {"sent": True}
    assert evaluate(boolean_only, repetitions=1)["missing"] == ["case:invalid_json"]
    float_counter = full({"handled": 3})
    float_counter["outcomes"]["invalid_json"] = {"sent": 1.5, "extra": 1.5}
    assert evaluate(float_counter)["missing"] == ["case:invalid_json"]
    string_counter = full({"handled": 3})
    string_counter["outcomes"]["invalid_json"] = {"sent": "3"}
    assert evaluate(string_counter)["missing"] == ["case:invalid_json"]
    negative_counter = full({"handled": 3})
    negative_counter["outcomes"]["invalid_json"] = {"sent": -3}
    assert evaluate(negative_counter)["missing"] == ["case:invalid_json"]
    cancelling = full({"handled": 3})
    cancelling["outcomes"]["invalid_json"] = {"sent": 5, "weird": -2}
    assert evaluate(cancelling)["missing"] == ["case:invalid_json"]
    # Controls: complete positive evidence passes, integer mismatches fail,
    # and absent cases stay missing.
    assert evaluate(full({"handled": 3})) == {
        "verdict": "PASS",
        "missing": [],
        "failed": [],
    }
    mismatch = full({"handled": 3})
    mismatch["outcomes"]["invalid_json"] = {"handled": 2}
    mismatch_result = evaluate(mismatch)
    assert mismatch_result["verdict"] == "FAIL"
    assert mismatch_result["failed"] == ["count:invalid_json"]
    absent = full({"handled": 3})
    del absent["outcomes"]["invalid_json"]
    absent_result = evaluate(absent)
    assert absent_result["verdict"] == "INCONCLUSIVE"
    assert absent_result["missing"] == ["case:invalid_json"]


def write_metrics_csv(measurement: Path, rows: list[tuple[str, str]]) -> None:
    lines = ["timestamp,rss_mb"] + [f"{stamp},{value}" for stamp, value in rows]
    measurement.joinpath("metrics.csv").write_text("\n".join(lines) + "\n", encoding="utf-8")


def write_k6_jsonl(measurement: Path, records: list[object]) -> None:
    measurement.joinpath("k6.jsonl").write_text(
        "\n".join(json.dumps(record) for record in records) + "\n", encoding="utf-8"
    )


@pytest.mark.parametrize(
    ("rows", "expected"),
    [
        pytest.param(
            [("0", "100"), ("1", "inf"), ("2", "50")],
            [(0.0, 100.0), (2.0, 50.0)],
            id="inf-value",
        ),
        pytest.param(
            [("0", "100"), ("1", "nan"), ("2", "50")],
            [(0.0, 100.0), (2.0, 50.0)],
            id="nan-value",
        ),
        pytest.param(
            [("inf", "100"), ("0", "50")],
            [(0.0, 50.0)],
            id="inf-timestamp",
        ),
        pytest.param(
            [("nan", "100"), ("1", "50")],
            [(1.0, 50.0)],
            id="nan-timestamp",
        ),
        pytest.param(
            [("0", "1" + "0" * 400), ("1", "50")],
            [(1.0, 50.0)],
            id="oversized-value",
        ),
    ],
)
def test_resource_samples_skip_unusable_csv_rows(
    tmp_path: Path, rows: list[tuple[str, str]], expected: list[tuple[float, float]]
) -> None:
    """Non-finite CSV samples are skipped while valid rows are retained."""
    measurement = tmp_path / "measurement"
    measurement.mkdir()
    write_metrics_csv(measurement, rows)
    assert campaign_evidence.resource_samples(tmp_path / "run", "rss_mb") == expected


@pytest.mark.parametrize(
    ("records", "expected"),
    [
        pytest.param(
            [
                {"metric": "benchmark_requests", "data": None},
                {"metric": "benchmark_requests", "data": {"time": 1, "value": 2}},
            ],
            {"effective_qps": [(1.0, 2.0)], "latency_p99_ms": []},
            id="null-data",
        ),
        pytest.param(
            [
                {"metric": "benchmark_requests", "data": "scalar"},
                {"metric": "benchmark_requests", "data": {"time": 1, "value": 2}},
            ],
            {"effective_qps": [(1.0, 2.0)], "latency_p99_ms": []},
            id="scalar-data",
        ),
        pytest.param(
            [
                {"metric": "benchmark_requests", "data": [{"time": 0, "value": 1}]},
                {"metric": "benchmark_requests", "data": {"time": 1, "value": 2}},
            ],
            {"effective_qps": [(1.0, 2.0)], "latency_p99_ms": []},
            id="list-data",
        ),
        pytest.param(
            [
                {
                    "metric": "benchmark_requests",
                    "data": {"time": 0, "value": True},
                },
                {"metric": "benchmark_requests", "data": {"time": 1, "value": 2}},
            ],
            {"effective_qps": [(1.0, 2.0)], "latency_p99_ms": []},
            id="boolean-value",
        ),
        pytest.param(
            [
                {
                    "metric": "benchmark_requests",
                    "data": {"time": 0, "value": float("nan")},
                },
                {"metric": "benchmark_requests", "data": {"time": 1, "value": 2}},
            ],
            {"effective_qps": [(1.0, 2.0)], "latency_p99_ms": []},
            id="nan-value",
        ),
        pytest.param(
            [
                {
                    "metric": "benchmark_requests",
                    "data": {"time": 0, "value": float("inf")},
                },
                {"metric": "benchmark_requests", "data": {"time": 1, "value": 2}},
            ],
            {"effective_qps": [(1.0, 2.0)], "latency_p99_ms": []},
            id="inf-value",
        ),
        pytest.param(
            [
                {
                    "metric": "benchmark_requests",
                    "data": {"time": float("inf"), "value": 1},
                },
                {"metric": "benchmark_requests", "data": {"time": 1, "value": 2}},
            ],
            {"effective_qps": [(1.0, 2.0)], "latency_p99_ms": []},
            id="inf-timestamp",
        ),
        pytest.param(
            [
                {
                    "metric": "benchmark_requests",
                    "data": {"time": "nan", "value": 1},
                },
                {"metric": "benchmark_requests", "data": {"time": 1, "value": 2}},
            ],
            {"effective_qps": [(1.0, 2.0)], "latency_p99_ms": []},
            id="nan-string-timestamp",
        ),
        pytest.param(
            [
                {
                    "metric": "benchmark_requests",
                    "data": {"time": 10**400, "value": 1},
                },
                {"metric": "benchmark_requests", "data": {"time": 1, "value": 2}},
            ],
            {"effective_qps": [(1.0, 2.0)], "latency_p99_ms": []},
            id="oversized-integer-timestamp",
        ),
        pytest.param(
            [
                {
                    "metric": "benchmark_requests",
                    "data": {"time": 0, "value": 10**400},
                },
                {"metric": "benchmark_requests", "data": {"time": 1, "value": 2}},
            ],
            {"effective_qps": [(1.0, 2.0)], "latency_p99_ms": []},
            id="oversized-integer-value",
        ),
        pytest.param(
            [
                {
                    "metric": "benchmark_requests",
                    "data": {"time": "1970-01-01T00:00:02Z", "value": 2},
                },
                {
                    "metric": "benchmark_latency",
                    "data": {"time": "1970-01-01T00:00:02Z", "value": 20},
                },
            ],
            {"effective_qps": [(2.0, 2.0)], "latency_p99_ms": [(2.0, 20.0)]},
            id="rfc3339-control",
        ),
    ],
)
def test_load_samples_skip_unusable_jsonl_records(
    tmp_path: Path, records: list[object], expected: dict[str, list[tuple[float, float]]]
) -> None:
    """Malformed JSONL points are skipped while valid records are retained."""
    measurement = tmp_path / "measurement"
    measurement.mkdir()
    write_k6_jsonl(measurement, records)
    assert campaign_evidence.load_samples(tmp_path / "run") == expected


def test_campaign_audit_classifies_malformed_confirmation_evidence() -> None:
    """Unusable capacity confirmation shapes are incomplete evidence, never a
    crash — and a string entry must not pass through str.count's substring
    semantics as three passes."""
    campaign = {
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
    }

    def capacities_with(confirmation: object) -> dict[str, object]:
        return {
            version: {
                "maximum_sustainable_qps": 100,
                "first_failed_qps": 150,
                "safety_limit_reached": False,
                "boundary_confirmed": True,
                "confirmation": confirmation,
            }
            for version in campaign_evidence.VERSIONS
        }

    for confirmation in (
        "confirmed",
        {"100": {"runs": 3}, "150": ["FAIL"] * 3},
        {"100": 3, "150": ["FAIL"] * 3},
        {"100": "PASSPASSPASS", "150": ["FAIL"] * 3},
    ):
        issues = campaign_evidence.audit_campaign(
            campaign, [], capacities_with(confirmation), {}, {}, {}
        )
        assert any(
            "pass confirmation is incomplete" in issue for issue in issues
        ), f"{confirmation!r} must read as incomplete pass evidence"

    # Control: complete confirmation evidence raises no capacity issue.
    issues = campaign_evidence.audit_campaign(
        campaign,
        [],
        capacities_with({"100": ["PASS"] * 3, "150": ["FAIL"] * 3}),
        {},
        {},
        {},
    )
    assert not any("capacity" in issue for issue in issues)


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


@pytest.mark.parametrize("repetitions", [1, 3])
def test_campaign_audit_accepts_only_complete_formal_evidence(
    repetitions: int,
) -> None:
    campaign_data = {
        "capacity": {"qps_resolution": 50, "confirm_repetitions": repetitions},
        "matrix": {
            "qps": [100, 200, 300, 400, 500],
            "repetitions": repetitions,
            "warmup_seconds": 180,
            "duration_seconds": 900,
        },
        "soak": {"warmup_seconds": 600, "duration_seconds": 14400},
        "recovery": {
            "repetitions": repetitions,
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
                "500": ["PASS"] * repetitions,
                "550": ["FAIL"] * repetitions,
            },
        }
        for version in campaign_evidence.VERSIONS
    }
    items = []
    for version in campaign_evidence.VERSIONS:
        for qps in campaign_data["matrix"]["qps"]:
            for repetition in range(1, repetitions + 1):
                items.append(
                    (
                        Path(f"/{version}/{qps}/{repetition}/run-result.json"),
                        {
                            "scenario": "matrix",
                            "version": version,
                            "label": f"qps-{qps}",
                            "qps": qps,
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
        for repetition in range(1, repetitions + 1):
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
        for repetition in range(1, repetitions + 1)
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
