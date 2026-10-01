from __future__ import annotations

import gzip
import json
import os
import sqlite3
import subprocess
import sys
import threading
import time
import urllib.request
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from types import SimpleNamespace

import pytest

BENCHMARK_DIR = Path(__file__).parents[1]
SINGLE_RUN_DIR = BENCHMARK_DIR / "single_run"
CAMPAIGN_DIR = BENCHMARK_DIR / "campaign"
sys.path.insert(0, str(CAMPAIGN_DIR))
sys.path.insert(0, str(SINGLE_RUN_DIR))

import aggregate_report
import campaign
import campaign_evaluation
import campaign_evidence
import campaign_manifest
import collect_metrics
import fault_injector
import mock_llm_server
import render_report
import run_regression
import validate_results


def campaign_data(tmp_path: Path) -> dict[str, object]:
    binary = tmp_path / "agentsight"
    config = tmp_path / "agentsight.json"
    metrics = tmp_path / "agentsight.metrics"
    runtime_log = tmp_path / "agentsight.log"
    binary.write_bytes(b"binary")
    metrics.write_text("", encoding="utf-8")
    runtime_log.write_text("", encoding="utf-8")
    config.write_text(
        json.dumps(
            {
                "cmdline": {
                    "allow": [
                        {
                            "rule": ["*python*", "*mock_llm_server.py*"],
                            "agent_name": "BenchmarkHarness",
                        }
                    ]
                }
            }
        ),
        encoding="utf-8",
    )
    return {
        "schema_version": 1,
        "campaign_id": "test",
        "versions": {
            name: {
                "commit": name,
                "binary": str(binary),
                "config": str(config),
                "pid": os.getpid(),
                "db": str(tmp_path / f"{name}.db"),
                "metrics_file": str(metrics),
                "log_file": str(runtime_log),
            }
            for name in ("baseline", "optimized")
        },
        "thresholds": {
            "min_throughput_ratio": 0.9,
            "min_http_success_rate": 0.99,
            "min_trace_completeness": 0.999,
            "min_token_accuracy": 1.0,
            "max_p99_ms": 100,
            "max_drop_rate": 0.01,
            "max_rss_mb": 100,
            "max_rss_slope_mb_per_hour": 10,
            "max_fd_slope_per_hour": 1,
            "max_thread_slope_per_hour": 1,
            "max_socket_slope_per_hour": 1,
            "max_recovery_seconds": 10,
        },
        "load": {"protocol": "sse", "payload_kb": 1, "chunks": 2, "chunk_bytes": 4},
        "smoke": {"qps": 10, "duration_seconds": 1},
        "capacity": {
            "qps_start": 100,
            "qps_resolution": 50,
            "qps_safety_max": 400,
            "pretest_warmup_seconds": 0,
            "pretest_duration_seconds": 1,
            "search_start_ratio": 0.8,
            "probe_warmup_seconds": 0,
            "probe_duration_seconds": 1,
            "confirm_warmup_seconds": 0,
            "confirm_duration_seconds": 1,
            "confirm_repetitions": 3,
        },
        "matrix": {
            "qps": [],
            "warmup_seconds": 0,
            "duration_seconds": 1,
            "repetitions": 3,
        },
        "soak": {"warmup_seconds": 0, "duration_seconds": 1},
        "recovery": {
            "stable_seconds": 1,
            "overload_seconds": 1,
            "recover_seconds": 1,
            "repetitions": 3,
            "tolerance_ratio": 0.1,
            "recovery_window_seconds": 1,
        },
        "fault": {
            "warmup_seconds": 0,
            "duration_seconds": 1,
            "repetitions_per_case": 1,
        },
    }


def complete_summary(qps: float = 100) -> dict[str, object]:
    return {
        "input_qps": qps,
        "effective_qps": qps,
        "http_success_rate": 1.0,
        "trace_completeness": 1.0,
        "token_accuracy": 1.0,
        "drop_rate": 0.0,
        "latency_ms": {"p99": 10.0},
        "resources": {
            "rss_mb": {"avg": 50.0, "p99": 55.0, "max": 60.0, "slope_per_hour": 1.0},
            "cpu_pct": {"avg": 20.0, "p95": 25.0},
            "channel_length": {"avg": 1.0, "max": 2.0},
            "connection_cache_bytes": {"avg": 10.0, "max": 20.0},
            "event_channel_bytes": {"avg": 10.0, "max": 20.0},
            "pending_genai_bytes": {"avg": 10.0, "max": 20.0},
        },
        "process_survived": True,
        "runtime_clean": True,
    }


def test_mock_response_events_are_deterministic_for_all_providers() -> None:
    for endpoint in mock_llm_server.ENDPOINTS:
        events = mock_llm_server.response_events(endpoint, "bench-1", 3, 5)
        assert len(events) == 4
        assert all(event["request_id"] == "bench-1" for event in events)
        assert events[-1].get("usage")


def test_request_id_prefers_body_then_header_then_fallback() -> None:
    assert (
        mock_llm_server.request_id_from(
            b'{"request_id":"body"}', {"X-Request-ID": "header"}
        )
        == "body"
    )
    assert (
        mock_llm_server.request_id_from(b"{}", {"X-Request-ID": "header"}) == "header"
    )
    assert mock_llm_server.request_id_from(b"not-json", {}) == "bench-missing"


def test_request_recorder_keeps_ids_on_disk_and_bounds_open_files(
    tmp_path: Path,
) -> None:
    recorder = mock_llm_server.RequestRecorder(tmp_path)
    recorder.record("run-1", "bench-run-1-a", 200)
    recorder.record("run-1", "bench-run-1-b", 503)
    first_handle = recorder.handle
    recorder.record("run-2", "bench-run-2-a", 200)

    assert first_handle is not None and first_handle.closed
    assert recorder.run_id == "run-2"
    assert validate_results.load_request_log(
        tmp_path / "run-1.jsonl", "bench-run-1-"
    ) == (
        {"bench-run-1-a", "bench-run-1-b"},
        {"bench-run-1-a"},
    )
    recorder.record("../invalid", "bench-invalid", 200)
    assert not (tmp_path.parent / "invalid.jsonl").exists()
    recorder.close()
    assert recorder.handle is None


def test_validate_results_matches_token_and_event_records(tmp_path: Path) -> None:
    load_path = tmp_path / "k6.jsonl"
    rows = []
    for request_id in ("bench-1", "bench-2", "bench-3"):
        rows.append(
            {
                "metric": "benchmark_requests",
                "data": {"value": 1, "tags": {"request_id": request_id}},
            }
        )
    rows.extend(
        [
            {
                "metric": "benchmark_http_success",
                "data": {"value": 1, "tags": {"request_id": "bench-1"}},
            },
            {
                "metric": "benchmark_http_success",
                "data": {"value": 1, "tags": {"request_id": "bench-2"}},
            },
        ]
    )
    load_path.write_text("\n".join(json.dumps(row) for row in rows), encoding="utf-8")

    db_path = tmp_path / "agentsight.db"
    with sqlite3.connect(db_path) as connection:
        connection.executescript(
            """
            CREATE TABLE token_records (request_id TEXT, input_tokens INTEGER, output_tokens INTEGER);
            INSERT INTO token_records VALUES ('bench-1', 12, 8);
            CREATE TABLE genai_events (call_id TEXT, trace_id TEXT, status TEXT, total_tokens INTEGER, event_json TEXT);
            INSERT INTO genai_events VALUES (NULL, NULL, 'complete', NULL, '{"request_id":"bench-2"}');
            INSERT INTO genai_events VALUES ('bench-extra', NULL, 'complete', 20, NULL);
            """
        )
    expected, successful = validate_results.load_expected(load_path)
    captured = validate_results.load_captured(db_path, "bench-")
    expected_only = validate_results.load_captured(db_path, "bench-", {"bench-1"})
    report = validate_results.make_report(expected, successful, captured, 20)
    assert report["sent"] == 3
    assert report["http_success"] == 2
    assert report["matched"] == 2
    assert report["complete"] == 2
    assert report["token_correct"] == 1
    assert report["expected_total_tokens"] == 40
    assert report["captured_total_tokens"] == 20
    assert report["missing_ids"] == ["bench-3"]
    assert report["extra_ids"] == ["bench-extra"]
    assert set(expected_only) == {"bench-1"}

    compressed_path = tmp_path / "k6.jsonl.gz"
    with gzip.open(compressed_path, "wt", encoding="utf-8") as handle:
        handle.write(load_path.read_text(encoding="utf-8"))
    assert validate_results.load_expected(compressed_path) == (expected, successful)
    assert render_report.summarize_load(compressed_path)["requests"] == 3


def test_validate_results_limits_timestamped_tables_to_current_run(
    tmp_path: Path,
) -> None:
    db_path = tmp_path / "agentsight.db"
    with sqlite3.connect(db_path) as connection:
        connection.executescript(
            """
            CREATE TABLE token_records (
                timestamp_ns INTEGER,
                request_id TEXT,
                input_tokens INTEGER,
                output_tokens INTEGER
            );
            INSERT INTO token_records VALUES (99, 'bench-current-old', 12, 8);
            INSERT INTO token_records VALUES (100, 'bench-current-token', 12, 8);
            CREATE TABLE genai_events (
                start_timestamp_ns INTEGER,
                call_id TEXT,
                status TEXT,
                total_tokens INTEGER,
                event_json TEXT
            );
            INSERT INTO genai_events VALUES (
                99, 'bench-current-old-event', 'complete', 20, '{}'
            );
            INSERT INTO genai_events VALUES (
                101, NULL, 'complete', 20,
                '{"request_id":"bench-current-event"}'
            );
            """
        )
    captured = validate_results.load_captured(db_path, "bench-current-", since_ns=100)
    assert set(captured) == {"bench-current-token", "bench-current-event"}
    incremental, genai_id, token_rowid, pending = (
        validate_results.load_captured_incremental(
            db_path, "bench-current-", 100, 0, 0, set()
        )
    )
    assert set(incremental) == {"bench-current-token", "bench-current-event"}
    assert genai_id == 0
    assert token_rowid == 2
    assert pending == set()


def test_streaming_validation_retains_pruned_and_updated_records(
    tmp_path: Path,
) -> None:
    db_path = tmp_path / "agentsight.db"
    with sqlite3.connect(db_path) as connection:
        connection.executescript(
            """
            CREATE TABLE genai_events (
                id INTEGER PRIMARY KEY,
                start_timestamp_ns INTEGER,
                call_id TEXT,
                trace_id TEXT,
                status TEXT,
                total_tokens INTEGER,
                event_json TEXT
            );
            INSERT INTO genai_events VALUES (
                1, 100, NULL, NULL, 'pending', NULL,
                '{"request_id":"bench-stream-1"}'
            );
            """
        )
    captured, genai_id, token_rowid, pending = (
        validate_results.load_captured_incremental(
            db_path, "bench-stream-", 100, 0, 0, set()
        )
    )
    assert captured == {"bench-stream-1": [("pending", None)]}
    assert genai_id == 1
    assert token_rowid == 0
    assert pending == {1}

    with sqlite3.connect(db_path) as connection:
        connection.executescript(
            """
            UPDATE genai_events
            SET status = 'complete', total_tokens = 20
            WHERE id = 1;
            INSERT INTO genai_events VALUES (
                2, 101, NULL, NULL, 'complete', 20,
                '{"request_id":"bench-stream-2"}'
            );
            """
        )
    incremental, genai_id, token_rowid, pending = (
        validate_results.load_captured_incremental(
            db_path, "bench-stream-", 100, genai_id, token_rowid, pending
        )
    )
    validate_results.merge_captured(captured, incremental)
    assert ("complete", 20) in captured["bench-stream-1"]
    assert captured["bench-stream-2"] == [("complete", 20)]
    assert genai_id == 2
    assert pending == set()

    with sqlite3.connect(db_path) as connection:
        connection.execute("DELETE FROM genai_events WHERE id = 1")
    expected = {"bench-stream-1", "bench-stream-2"}
    report = validate_results.make_report(expected, expected, captured, 20)
    assert report["completeness_ratio"] == 1.0
    assert report["token_accuracy"] == 1.0


def test_streaming_validation_waits_for_load_completion_marker(
    tmp_path: Path,
) -> None:
    load_path = tmp_path / "k6.jsonl"
    request_log = tmp_path / "request-log.jsonl"
    marker = tmp_path / "load-complete"
    db_path = tmp_path / "agentsight.db"
    load_path.write_text(
        json.dumps(
            {
                "metric": "benchmark_http_success",
                "data": {"value": 1, "tags": {}},
            }
        ),
        encoding="utf-8",
    )
    request_log.write_text(
        json.dumps({"request_id": "bench-live-1", "http_status": 200}) + "\n",
        encoding="utf-8",
    )
    with sqlite3.connect(db_path) as connection:
        connection.executescript(
            """
            CREATE TABLE genai_events (
                id INTEGER PRIMARY KEY,
                start_timestamp_ns INTEGER,
                call_id TEXT,
                status TEXT,
                total_tokens INTEGER,
                event_json TEXT
            );
            INSERT INTO genai_events VALUES (
                1, 1, NULL, 'complete', 20,
                '{"request_id":"bench-live-1"}'
            );
            """
        )
    timer = threading.Timer(0.02, marker.touch)
    timer.start()
    try:
        report = validate_results.wait_for_streaming_report(
            db_path,
            load_path,
            marker,
            "bench-live-",
            20,
            0.999,
            0.1,
            0.005,
            100,
            request_log,
        )
    finally:
        timer.cancel()
    assert report["sent"] == 1
    assert report["complete"] == 1
    assert report["validation_mode"] == "streaming_incremental"
    assert report["retention_safe"] is True


def test_validate_results_handles_invalid_json_and_empty_expected(
    tmp_path: Path,
) -> None:
    path = tmp_path / "empty.jsonl"
    path.write_text("not-json\n[]\n", encoding="utf-8")
    assert validate_results.load_expected(path) == (set(), set())
    report = validate_results.make_report(set(), set(), {}, None)
    assert report["completeness_ratio"] == 0


def test_validate_results_extracts_ids_from_nested_raw_body() -> None:
    event = {
        "LLMCall": {
            "request": {
                "raw_body": json.dumps(
                    {"request_id": "bench-raw-body", "model": "test-model"}
                )
            }
        }
    }
    assert validate_results.find_request_ids(event) == {"bench-raw-body"}


def test_procfs_collector_reads_process_and_internal_metrics(tmp_path: Path) -> None:
    metrics = tmp_path / "metrics.txt"
    metrics.write_text(
        "channel_length=7\neviction_count=2\ncompleted=11\nignored=3\n",
        encoding="utf-8",
    )
    status = collect_metrics.read_status(os.getpid())
    assert status["Threads"] >= 1
    assert collect_metrics.read_cpu_ticks(os.getpid()) >= 0
    assert collect_metrics.fd_count(os.getpid()) >= 1
    assert collect_metrics.socket_count(os.getpid()) >= 0
    assert collect_metrics.read_internal_metrics(metrics) == {
        "channel_length": "7",
        "eviction_count": "2",
        "completed": "11",
    }
    metrics.write_text(
        "# TYPE agentsight_event_channel_length gauge\n"
        "agentsight_event_channel_length 9\n"
        "agentsight_connection_evictions_total 4\n"
        "agentsight_events_completed_total 13\n"
        "agentsight_stage_calls_total{stage=\"parser\"} 3\n",
        encoding="utf-8",
    )
    assert collect_metrics.read_internal_metrics(metrics) == {
        "channel_length": "9",
        "eviction_count": "4",
        "completed": "13",
    }
    row, _, _ = collect_metrics.sample(
        os.getpid(),
        collect_metrics.read_cpu_ticks(os.getpid()),
        time.monotonic(),
        500,
        metrics,
    )
    assert row["input_qps"] == 500
    assert row["channel_length"] == "9"


def test_report_records_qps_cpu_rss_drop_rate_and_validation(tmp_path: Path) -> None:
    load_path = tmp_path / "k6.jsonl"
    load_path.write_text(
        "\n".join(
            [
                json.dumps({"metric": "benchmark_requests", "data": {"value": 1}}),
                json.dumps({"metric": "benchmark_requests", "data": {"value": 1}}),
                json.dumps({"metric": "benchmark_http_success", "data": {"value": 1}}),
                json.dumps({"metric": "benchmark_latency", "data": {"value": 12.0}}),
                json.dumps({"metric": "benchmark_latency", "data": {"value": 20.0}}),
                json.dumps({"metric": "http_req_duration", "data": {"value": 15.0}}),
                "not-json",
            ]
        ),
        encoding="utf-8",
    )
    metrics_path = tmp_path / "metrics.csv"
    metrics_path.write_text(
        "timestamp,input_qps,cpu_pct,rss_mb,threads,file_descriptors,"
        "ring_buffer_dropped,channel_dropped,completed\n"
        "1,100,10,20,2,5,0,0,10\n"
        "2,100,30,40,4,7,0,2,20\n",
        encoding="utf-8",
    )
    validation = {
        "matched": 2,
        "complete": 2,
        "completeness_ratio": 1,
        "token_correct": 2,
        "expected_total_tokens": 40,
        "captured_total_tokens": 20,
    }

    load = render_report.summarize_load(load_path)
    resources = render_report.summarize_resources(metrics_path)
    qps_resources = render_report.summarize_qps_resources(metrics_path)
    drops = render_report.summarize_drops(metrics_path, load["requests"])
    markdown = render_report.render_markdown(
        protocol="sse",
        qps="100",
        duration="30",
        load=load,
        resources=resources,
        qps_resources=qps_resources,
        drops=drops,
        validation=validation,
        load_artifact=load_path,
        load_log_artifact=None,
        metrics_artifact=metrics_path,
    )

    assert load["requests"] == 2
    assert load["available"] is True
    assert load["http_success"] == 1
    assert load["latency"]["p95"] == 20
    assert resources["cpu_pct"]["avg"] == 20
    assert qps_resources["100"]["cpu_pct"]["max"] == 30
    assert qps_resources["100"]["rss_mb"]["avg"] == 30
    assert drops["channel_dropped"] == 2
    assert drops["drop_rate"] == pytest.approx(16.666666, rel=1e-5)
    assert "# AgentSight benchmark report" in markdown
    assert "| Input QPS | Samples | CPU Avg | CPU P95 | RSS Avg | RSS P99 | RSS Max |" in markdown
    assert "| 100 | 2 | 20.00% | 30.00% | 30.00 MB | 40.00 MB | 40.00 MB |" in markdown
    assert "| 2 | 1 | 1 | 0 | 50.00% | 0.07 req/s |" in markdown
    assert "16.67%" in markdown
    assert "50.00%" in markdown
    assert "| 1.33 | 0.67 |" in markdown
    assert "## Regression coverage" in markdown
    assert "sustained_load_never_exceeds_byte_budget" in markdown
    assert "connection_cache_bytes" not in markdown


def test_report_computes_nearest_rank_slopes_and_machine_summary(
    tmp_path: Path,
) -> None:
    metrics_path = tmp_path / "metrics.csv"
    metrics_path.write_text(
        "timestamp,input_qps,process_alive,cpu_pct,rss_mb,threads,file_descriptors,"
        "active_connections,ring_buffer_dropped,channel_dropped,completed\n"
        "0,100,1,10,20,2,5,1,0,0,0\n"
        "1800,100,1,20,30,2,5,1,0,1,50\n"
        "3600,100,1,30,40,2,5,1,0,2,100\n",
        encoding="utf-8",
    )
    resources = render_report.summarize_resources(metrics_path)
    assert resources["rss_mb"]["p50"] == 30
    assert resources["rss_mb"]["p99"] == 40
    assert resources["rss_mb"]["slope_per_hour"] == pytest.approx(20)
    assert resources["rss_mb"]["rolling_5m_max_increase"] == 0
    load = {
        "requests": 300,
        "http_success": 299,
        "latency": render_report.stats([1, 2, 3]),
    }
    drops = render_report.summarize_drops(metrics_path, 300)
    summary = render_report.build_summary(
        protocol="sse",
        qps="100",
        duration="3",
        load=load,
        resources=resources,
        drops=drops,
        validation={
            "match_ratio": 1.0,
            "completeness_ratio": 0.999,
            "token_accuracy": 1.0,
            "expected_total_tokens": 6000,
            "captured_total_tokens": 5900,
        },
    )
    assert summary["effective_qps"] == 100
    assert summary["tokens_per_second"]["generated"] == 2000
    assert summary["tokens_per_second"]["captured"] == pytest.approx(1966.6667)
    assert summary["process_survived"] is True
    assert summary["drop_rate"] == pytest.approx(0.0196078, rel=1e-5)
    assert summary["capture_loss_rate"] == pytest.approx(0.001)
    assert (
        summary["drop_rate_source"] == "max(internal_counters,capture_reconciliation)"
    )


def test_drop_rate_is_missing_without_internal_counters(tmp_path: Path) -> None:
    metrics_path = tmp_path / "metrics.csv"
    metrics_path.write_text("timestamp,input_qps,rss_mb\n1,10,20\n", encoding="utf-8")
    assert render_report.summarize_drops(metrics_path, 10)["drop_rate"] is None


def test_drop_rate_requires_two_counter_samples(tmp_path: Path) -> None:
    metrics_path = tmp_path / "metrics.csv"
    metrics_path.write_text(
        "timestamp,ring_buffer_dropped,channel_dropped,completed\n"
        "1,7,3,100\n",
        encoding="utf-8",
    )

    drops = render_report.summarize_drops(metrics_path, 10)

    assert drops == {
        "ring_buffer_dropped": None,
        "channel_dropped": None,
        "completed": None,
        "total_dropped": None,
        "drop_rate": None,
    }


def test_summary_uses_end_to_end_loss_without_internal_counters() -> None:
    summary = render_report.build_summary(
        protocol="sse",
        qps="100",
        duration="10",
        load={
            "requests": 1000,
            "http_success": 1000,
            "timeouts": 0,
            "latency": render_report.stats([1.0]),
        },
        resources={
            name: render_report.stats([]) for name in render_report.RESOURCE_METRICS
        },
        drops={
            "ring_buffer_dropped": None,
            "channel_dropped": None,
            "completed": None,
            "total_dropped": None,
            "drop_rate": None,
        },
        validation={"completeness_ratio": 0.997, "match_ratio": 0.998},
    )
    assert summary["capture_loss_rate"] == pytest.approx(0.003)
    assert summary["drop_rate"] == pytest.approx(0.003)
    assert summary["drop_rate_source"] == "capture_reconciliation"


def test_report_does_not_compute_tps_for_non_positive_duration() -> None:
    empty_resources = {
        name: render_report.stats([]) for name in render_report.RESOURCE_METRICS
    }
    load = {
        "available": True,
        "requests": 1,
        "http_success": 1,
        "timeouts": 0,
        "throughput": None,
        "latency": render_report.stats([1.0]),
        "latency_label": "custom latency (ms)",
    }
    markdown = render_report.render_markdown(
        protocol="sse",
        qps="1",
        duration="-1",
        load=load,
        resources=empty_resources,
        qps_resources={},
        drops={
            "ring_buffer_dropped": None,
            "channel_dropped": None,
            "completed": None,
            "total_dropped": None,
            "drop_rate": None,
        },
        validation={"expected_total_tokens": 20, "captured_total_tokens": 20},
        load_artifact=None,
        load_log_artifact=None,
        metrics_artifact=None,
    )

    assert (
        "## Token throughput\n\n"
        "| Generated tokens/s | Captured tokens/s |\n"
        "| ---: | ---: |\n"
        "| — | — |"
    ) in markdown


def test_render_report_cli_handles_missing_artifacts(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    output = tmp_path / "report.md"
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "render_report.py",
            "--output",
            str(output),
            "--protocol",
            "h2",
            "--qps",
            "10",
            "--duration",
            "5",
        ],
    )
    assert render_report.main() == 0
    report = output.read_text(encoding="utf-8")
    assert "No process metrics collected" in report
    assert "No load summary" in report


def test_mock_server_serves_json_and_sse_over_https(tmp_path: Path) -> None:
    cert = tmp_path / "server.crt"
    key = tmp_path / "server.key"
    subprocess.run(
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
            "-keyout",
            str(key),
            "-out",
            str(cert),
        ],
        check=True,
        capture_output=True,
    )
    server = mock_llm_server.BenchmarkHTTPServer(
        ("127.0.0.1", 0), mock_llm_server.BenchmarkHandler
    )
    recorder = mock_llm_server.RequestRecorder(tmp_path / "request-logs")
    server.settings = SimpleNamespace(
        chunks=2,
        chunk_bytes=4,
        chunk_delay=0,
        sse=True,
        request_recorder=recorder,
    )
    server.verbose = False
    tls_context = __import__("ssl").SSLContext(__import__("ssl").PROTOCOL_TLS_SERVER)
    tls_context.load_cert_chain(cert, key)
    tls_context.set_alpn_protocols(["http/1.1"])
    server.socket = tls_context.wrap_socket(server.socket, server_side=True)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        context = __import__("ssl")._create_unverified_context()
        with urllib.request.urlopen(
            f"https://127.0.0.1:{port}/healthz", context=context
        ) as response:
            assert response.read() == b"OK"
        request = urllib.request.Request(
            f"https://127.0.0.1:{port}/v1/chat/completions",
            data=b'{"request_id":"bench-http","stream":true}',
            headers={
                "Content-Type": "application/json",
                "X-Benchmark-Run-ID": "http-test",
            },
            method="POST",
        )
        with urllib.request.urlopen(request, context=context) as response:
            body = response.read().decode()
        assert "bench-http" in body
        assert "[DONE]" in body
        server.settings.sse = False
        with urllib.request.urlopen(request, context=context) as response:
            json_body = json.loads(response.read())
        assert json_body["request_id"] == "bench-http"
        with pytest.raises(urllib.error.HTTPError) as error:
            urllib.request.urlopen(f"https://127.0.0.1:{port}/missing", context=context)
        assert error.value.code == 404
        raw_response = fault_injector.exchange(
            "127.0.0.1", port, fault_injector.post(b"{}"), 1
        )
        assert " 200 " in raw_response
        assert fault_injector.health_check("127.0.0.1", port, 1)
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        recorder.close()
    expected, successful = validate_results.load_request_log(
        tmp_path / "request-logs/http-test.jsonl"
    )
    assert expected == {"bench-http"}
    assert successful == {"bench-http"}


def test_runner_shell_contract_is_valid() -> None:
    runner = SINGLE_RUN_DIR / "run.sh"
    check = subprocess.run(
        ["bash", "-n", str(runner)], capture_output=True, text=True, check=False
    )
    assert check.returncode == 0, check.stderr
    help_result = subprocess.run(
        [str(runner), "--help"], capture_output=True, text=True, check=False
    )
    assert help_result.returncode == 0
    assert "--userspace" in help_result.stdout
    assert "--external-server" in help_result.stdout
    assert "--fault-count" in help_result.stdout
    assert "--max-results-gb" in help_result.stdout
    assert "--request-log-dir" in help_result.stdout
    runner_text = runner.read_text(encoding="utf-8")
    load_text = (SINGLE_RUN_DIR / "load/k6.js").read_text(encoding="utf-8")
    assert 'RUN_ID="$RUN_ID"' in runner_text
    assert '--prefix "bench-$RUN_ID-" --since-ns "$VALIDATION_SINCE_NS"' in runner_text
    assert '--completion-marker "$VALIDATION_MARKER"' in runner_text
    assert '--request-log "$REQUEST_LOG"' in runner_text
    assert 'wait "$VALIDATOR_PID"' in runner_text
    assert 'LOAD_RESULTS="$OUTPUT_DIR/k6.jsonl.gz"' in runner_text
    assert 'gzip -1 <"$K6_PIPE"' in runner_text
    assert 'BENCHMARK_MAX_VUS="$MAX_K6_VUS"' in runner_text
    assert '--safety-output "$OUTPUT_DIR/safety-stop.json"' in runner_text
    assert runner_text.index("VALIDATOR_PID=$!") < runner_text.index("k6 run")
    assert 'if [[ "$EXTERNAL_SERVER" -eq 0 ]]' in runner_text
    assert "`bench-${runId}-${__VU}-${__ITER}-${Date.now()}`" in load_text
    assert "BENCHMARK_MAX_VUS || 256" in load_text
    assert "'X-Benchmark-Run-ID': runId" in load_text
    assert "X-Request-ID" not in load_text
    assert "tags:" not in load_text
    assert "requestCount.add(1," not in load_text
    assert "requestLatency.add(response.timings.duration," not in load_text


def test_every_bpf_ring_reservation_failure_is_counted() -> None:
    bpf_dir = BENCHMARK_DIR.parents[1] / "src/bpf"
    sources = (
        "proctrace.bpf.c",
        "procmon.bpf.c",
        "filewatch.bpf.c",
        "filewrite.bpf.c",
        "udpdns.bpf.c",
        "sslsniff.bpf.c",
        "tcpsniff.bpf.c",
    )
    for name in sources:
        text = (bpf_dir / name).read_text(encoding="utf-8")
        assert text.count("bpf_ringbuf_reserve(") == text.count(
            "record_ring_buffer_drop();"
        ), name


def test_reproducer_builds_frozen_inputs_and_invokes_runner(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    reproducer = CAMPAIGN_DIR / "reproduce_campaign.sh"
    syntax = subprocess.run(
        ["bash", "-n", str(reproducer)], capture_output=True, text=True, check=False
    )
    assert syntax.returncode == 0, syntax.stderr
    help_result = subprocess.run(
        [str(reproducer), "--help"], capture_output=True, text=True, check=False
    )
    assert help_result.returncode == 0
    assert "--baseline-ref" in help_result.stdout
    assert "--optimized-ref" in help_result.stdout
    assert "--allow-identical-versions" in help_result.stdout

    fake_bin = tmp_path / "bin"
    fake_repo = tmp_path / "repo"
    fake_bin.mkdir()
    fake_repo.mkdir()
    sudo_log = tmp_path / "sudo.log"
    fake_git = fake_bin / "git"
    fake_git.write_text(
        "#!/bin/sh\n"
        'case "$*" in\n'
        "  *'rev-parse --show-toplevel'*) echo \"$FAKE_REPO\" ;;\n"
        "  *'rev-parse --verify baseline^{commit}'*) printf '%040d\\n' 1 ;;\n"
        "  *'rev-parse --verify optimized^{commit}'*) printf '%040d\\n' 2 ;;\n"
        "  *'worktree add --detach'*)\n"
        '    mkdir -p "$6/src/agentsight"\n'
        '    printf \'%s\\n\' \'{"cmdline":{"allow":[]}}\' > "$6/src/agentsight/agentsight.json"\n'
        "    printf '%s\\n' 'version = 4' > \"$6/src/agentsight/Cargo.lock\" ;;\n"
        "  *'diff -- src/agentsight/Cargo.lock'*) printf '%s\\n' 'fake lock diff' ;;\n"
        "  *'worktree remove --force'*) : ;;\n"
        '  *) echo "unexpected git command: $*" >&2; exit 9 ;;\n'
        "esac\n",
        encoding="utf-8",
    )
    fake_rustup = fake_bin / "rustup"
    fake_rustup.write_text(
        "#!/bin/sh\n"
        'case "$*" in\n'
        "  *--locked*) echo 'error: the lock file needs to be updated but --locked was passed'; exit 101 ;;\n"
        "esac\n"
        "printf '%s\\n' '# resolved' >> \"$PWD/Cargo.lock\"\n"
        'mkdir -p "$CARGO_TARGET_DIR/release"\n'
        "printf '%s\\n' '#!/bin/sh' 'exit 0' > \"$CARGO_TARGET_DIR/release/agentsight\"\n"
        'chmod +x "$CARGO_TARGET_DIR/release/agentsight"\n',
        encoding="utf-8",
    )
    fake_sudo = fake_bin / "sudo"
    fake_sudo.write_text(
        "#!/bin/sh\n"
        "if [ \"$1\" = '-v' ]; then\n"
        "  printf '%s\\n' 'AUTH' >> \"$FAKE_SUDO_LOG\"\n"
        "  exit 0\n"
        "fi\n"
        'printf \'RUN %s\\n\' "$*" >> "$FAKE_SUDO_LOG"\n',
        encoding="utf-8",
    )
    fake_python = fake_bin / "python3"
    fake_python.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' '[CLOCK] BPF timestamp domains aligned (skew=0.000s)'\n",
        encoding="utf-8",
    )
    fake_df = fake_bin / "df"
    fake_df.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n"
        "printf '%s\\n' '/dev/fake 104857600 0 104857600 0% /'\n",
        encoding="utf-8",
    )
    for executable in (fake_git, fake_rustup, fake_sudo, fake_python, fake_df):
        executable.chmod(0o755)

    monkeypatch.setenv("FAKE_REPO", str(fake_repo))
    monkeypatch.setenv("FAKE_SUDO_LOG", str(sudo_log))
    monkeypatch.setenv("PATH", f"{fake_bin}:{os.environ['PATH']}")
    rejected = subprocess.run(
        [
            str(reproducer),
            "--baseline-ref",
            "baseline",
            "--optimized-ref",
            "baseline",
            "--mode",
            "formal",
            "--results",
            str(tmp_path / "rejected"),
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    assert rejected.returncode == 2
    assert "must resolve to different commits" in rejected.stderr

    results = tmp_path / "results"
    completed = subprocess.run(
        [
            str(reproducer),
            "--baseline-ref",
            "baseline",
            "--optimized-ref",
            "baseline",
            "--mode",
            "formal",
            "--allow-identical-versions",
            "--results",
            str(results),
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    assert completed.returncode == 0, completed.stderr
    prepared = json.loads((results / "inputs/campaign.json").read_text())
    assert prepared["smoke"]["qps"] == 10
    assert prepared["comparison_mode"] == "aa_calibration"
    assert prepared["versions"]["baseline"]["commit"] == "0" * 39 + "1"
    assert prepared["versions"]["baseline"]["source_ref"] == "baseline"
    assert prepared["versions"]["optimized"]["commit"] == "0" * 39 + "1"
    assert prepared["versions"]["optimized"]["source_ref"] == "baseline"
    assert (results / "inputs/baseline/agentsight").is_file()
    assert (results / "inputs/optimized/agentsight.json").is_file()
    provenance = json.loads(
        (results / "inputs/baseline/build-provenance.json").read_text()
    )
    assert provenance["lockfile_updated"] is True
    assert provenance["committed_lock_sha256"] != provenance["resolved_lock_sha256"]
    assert (results / "inputs/baseline/Cargo.lock.diff").read_text() == (
        "fake lock diff\n"
    )
    assert json.loads((results / "inputs/build-settings.json").read_text()) == {
        "build_jobs": 2,
        "campaign_template": str(CAMPAIGN_DIR / "campaign.example.json"),
        "comparison_mode": "aa_calibration",
        "rust_toolchain": "1.89.0",
    }
    sudo_lines = sudo_log.read_text(encoding="utf-8").splitlines()
    assert sudo_lines[0] == "AUTH"
    invocation = sudo_lines[-1]
    assert "run_campaign.py" in invocation
    assert "--allow-identical-versions" in invocation
    assert "--resume" in invocation
    assert "unshare --mount --propagation private" in invocation
    assert f"--isolated-storage-root {results}/runtime/storage" in invocation
    assert "AGENTSIGHT_CARGO_JOBS=2" in invocation
    assert completed.stdout.index("[SUDO]") < completed.stdout.index("[BUILD]")


def test_reproducer_rejects_nondefault_database_before_build(tmp_path: Path) -> None:
    reproducer = CAMPAIGN_DIR / "reproduce_campaign.sh"
    result = subprocess.run(
        [str(reproducer), "--db", str(tmp_path / "events.db")],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 2
    assert (
        "--db must remain /var/log/sysak/.agentsight/genai_events.db" in result.stderr
    )
    assert "[BUILD]" not in result.stdout
    assert "[SUDO]" not in result.stdout


def test_fault_injector_defines_all_required_cases(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    assert set(fault_injector.cases(8)) == {
        "invalid_json",
        "invalid_chunk_size",
        "content_length_mismatch",
        "truncated_body",
        "truncated_sse",
        "invalid_utf8",
        "binary_body",
        "oversized_body",
    }

    class Connection:
        def close(self) -> None:
            pass

    monkeypatch.setattr(
        fault_injector, "exchange", lambda *args, **kwargs: "HTTP/1.1 400 Bad Request"
    )
    monkeypatch.setattr(fault_injector, "health_check", lambda *args: True)
    monkeypatch.setattr(
        fault_injector, "tls_socket", lambda *args, **kwargs: Connection()
    )
    report = fault_injector.inject("localhost", 443, 2, 8, 1, os.getpid())
    assert report["server_healthy_after"] is True
    assert report["process_alive_after"] is True
    assert all(sum(outcomes.values()) == 2 for outcomes in report["outcomes"].values())


def test_campaign_validation_manifest_and_evaluation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    for version in data["versions"].values():
        version.pop("metrics_file")
        version.pop("log_file")
    campaign.validate_campaign(data)
    assert campaign_manifest.sha256(Path(data["versions"]["baseline"]["binary"]))
    assert (
        campaign_evaluation.evaluate(complete_summary(), data["thresholds"])["verdict"]
        == "PASS"
    )
    failed = complete_summary()
    failed["latency_ms"]["p99"] = 1000
    assert campaign_evaluation.evaluate(failed, data["thresholds"])["verdict"] == "FAIL"
    incomplete = complete_summary()
    incomplete["trace_completeness"] = None
    assert (
        campaign_evaluation.evaluate(incomplete, data["thresholds"])["verdict"]
        == "INCONCLUSIVE"
    )
    missing_token = complete_summary()
    missing_token["token_accuracy"] = None
    token_evaluation = campaign_evaluation.evaluate(
        missing_token, data["thresholds"]
    )
    assert token_evaluation["verdict"] == "INCONCLUSIVE"
    assert token_evaluation["missing"] == ["token_accuracy"]
    wrong_token = complete_summary()
    wrong_token["token_accuracy"] = 0.999
    token_evaluation = campaign_evaluation.evaluate(wrong_token, data["thresholds"])
    assert token_evaluation["verdict"] == "FAIL"
    assert token_evaluation["failed"] == ["token_accuracy"]
    startup_growth = complete_summary()
    startup_growth["resources"]["rss_mb"]["slope_per_hour"] = 999
    startup_growth["resources"].update(
        {
            "file_descriptors": {"slope_per_hour": 0.0},
            "threads": {"slope_per_hour": 0.0},
            "active_connections": {"slope_per_hour": 0.0},
        }
    )
    smoke = campaign_evaluation.evaluate(
        startup_growth, data["thresholds"], scenario="smoke"
    )
    assert smoke["verdict"] == "PASS"
    assert "rss_slope_mb_per_hour" not in smoke["checks"]
    capacity_evaluation = campaign_evaluation.evaluate(
        startup_growth, data["thresholds"]
    )
    assert capacity_evaluation["verdict"] == "PASS"
    assert "rss_slope_mb_per_hour" not in capacity_evaluation["checks"]
    soak = campaign_evaluation.evaluate(
        startup_growth, data["thresholds"], scenario="soak"
    )
    assert soak["verdict"] == "FAIL"
    assert "rss_slope_mb_per_hour" in soak["checks"]

    monkeypatch.setattr(
        campaign_manifest,
        "environment_manifest",
        lambda path, value: {"frozen": campaign_manifest.frozen_inputs(path, value)},
    )
    config_path = tmp_path / "campaign.json"
    config_path.write_text(json.dumps(data), encoding="utf-8")
    results = tmp_path / "results"
    campaign_manifest.ensure_manifest(results, config_path, data)
    campaign_manifest.ensure_manifest(results, config_path, data)
    data["thresholds"]["max_rss_mb"] = 200
    with pytest.raises(ValueError, match="changed after freeze"):
        campaign_manifest.ensure_manifest(results, config_path, data)


def test_campaign_rejects_bad_shapes(tmp_path: Path) -> None:
    data = campaign_data(tmp_path)
    data["matrix"]["qps"] = [1, 2]
    with pytest.raises(ValueError, match="five unique"):
        campaign.validate_campaign(data)
    data = campaign_data(tmp_path)
    del data["thresholds"]["max_p99_ms"]
    with pytest.raises(ValueError, match="missing frozen"):
        campaign.validate_campaign(data)
    data = campaign_data(tmp_path)
    data["thresholds"]["min_token_accuracy"] = 1.01
    with pytest.raises(ValueError, match="ratio between 0 and 1"):
        campaign.validate_campaign(data)


def test_run_once_preserves_metadata_and_verdict(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    commands: list[list[str]] = []

    def fake_run(command: list[str], **_: object) -> SimpleNamespace:
        commands.append(command)
        assert "--external-server" in command
        assert command[command.index("--max-results-gb") + 1] == "30"
        assert command[command.index("--max-k6-vus") + 1] == "256"
        assert command[command.index("--request-log-dir") + 1].endswith(
            "runtime/mock-server/request-logs"
        )
        output = Path(command[command.index("--output-dir") + 1])
        output.mkdir(parents=True, exist_ok=True)
        if output.name == "measurement":
            (output / "run-summary.json").write_text(
                json.dumps(complete_summary(100)), encoding="utf-8"
            )
        return SimpleNamespace(returncode=0)

    monkeypatch.setattr(campaign.subprocess, "run", fake_run)
    result = campaign.run_once(
        data,
        tmp_path / "results",
        "baseline",
        "matrix",
        "qps-100",
        100,
        1,
        1,
        2,
        os.getpid(),
    )
    assert result["evaluation"]["verdict"] == "PASS"
    assert result["warmup_seconds"] == 1
    assert result["summary"]["runtime_clean"] is True
    assert result["process"]["pid"] == os.getpid()
    assert result["metrics_enabled"] is True
    assert "--db" not in commands[0]
    assert "--metrics-file" not in commands[0]
    assert "--db" in commands[1]
    assert "--metrics-file" in commands[1]
    assert list((tmp_path / "results").glob("runs/**/run-result.json"))
    with pytest.raises(FileExistsError, match="new campaign ID"):
        campaign.run_once(
            data,
            tmp_path / "results",
            "baseline",
            "matrix",
            "qps-100",
            100,
            1,
            1,
            2,
            os.getpid(),
        )
    resumed = campaign.run_once(
        data,
        tmp_path / "results",
        "baseline",
        "matrix",
        "qps-100",
        100,
        1,
        1,
        2,
        os.getpid(),
        resume=True,
    )
    assert resumed["started_at_unix"] == result["started_at_unix"]
    assert resumed["evaluation"]["verdict"] == "PASS"

    data["versions"]["baseline"].pop("metrics_file")
    disabled = campaign.run_once(
        data,
        tmp_path / "results",
        "baseline",
        "metrics_off",
        "qps-100",
        100,
        1,
        0,
        1,
        os.getpid(),
    )
    assert disabled["metrics_enabled"] is False
    assert "--metrics-file" not in commands[-1]


def test_run_once_continues_after_a_measurable_warmup_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    calls = 0

    def fake_run(command: list[str], **_: object) -> SimpleNamespace:
        nonlocal calls
        calls += 1
        output = Path(command[command.index("--output-dir") + 1])
        output.mkdir(parents=True, exist_ok=True)
        (output / "run-summary.json").write_text(
            json.dumps(complete_summary(100)), encoding="utf-8"
        )
        return SimpleNamespace(returncode=1 if output.name == "warmup" else 0)

    monkeypatch.setattr(campaign.subprocess, "run", fake_run)
    result = campaign.run_once(
        data,
        tmp_path / "results",
        "baseline",
        "capacity",
        "pretest-100",
        100,
        1,
        1,
        1,
        os.getpid(),
    )
    assert calls == 2
    assert result["harness_exit_code"] == 0
    assert result["evaluation"]["verdict"] == "PASS"


def test_run_once_aborts_after_a_warmup_safety_stop(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)

    def fake_run(command: list[str], **_: object) -> SimpleNamespace:
        output = Path(command[command.index("--output-dir") + 1])
        output.mkdir(parents=True, exist_ok=True)
        (output / "safety-stop.json").write_text(
            json.dumps({"reason": "available_memory"}), encoding="utf-8"
        )
        return SimpleNamespace(returncode=campaign.SAFETY_EXIT_CODE)

    monkeypatch.setattr(campaign.subprocess, "run", fake_run)
    with pytest.raises(RuntimeError, match="during warmup.*available_memory"):
        campaign.run_once(
            data,
            tmp_path / "results",
            "baseline",
            "capacity",
            "pretest-100",
            100,
            1,
            1,
            1,
            os.getpid(),
        )


def test_run_once_aborts_the_campaign_after_a_safety_stop(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    results = tmp_path / "results"

    def fake_run(command: list[str], **_: object) -> SimpleNamespace:
        output = Path(command[command.index("--output-dir") + 1])
        output.mkdir(parents=True, exist_ok=True)
        (output / "run-summary.json").write_text(
            json.dumps(complete_summary()), encoding="utf-8"
        )
        (output / "safety-stop.json").write_text(
            json.dumps({"reason": "available_memory"}), encoding="utf-8"
        )
        return SimpleNamespace(returncode=campaign.SAFETY_EXIT_CODE)

    monkeypatch.setattr(campaign.subprocess, "run", fake_run)
    with pytest.raises(RuntimeError, match="available_memory"):
        campaign.run_once(
            data,
            results,
            "baseline",
            "matrix",
            "qps-100",
            100,
            1,
            0,
            1,
            os.getpid(),
        )
    result = json.loads(
        next(results.glob("runs/**/run-result.json")).read_text(encoding="utf-8")
    )
    assert result["harness_exit_code"] == campaign.SAFETY_EXIT_CODE
    assert result["summary"]["safety_stop"]["reason"] == "available_memory"


def test_run_once_archives_an_incomplete_attempt(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    results = tmp_path / "results"
    incomplete = results / "runs/matrix/baseline/qps-200/rep-1"
    incomplete.mkdir(parents=True)
    (incomplete / "partial.log").write_text("interrupted", encoding="utf-8")

    def fake_run(command: list[str], **_: object) -> SimpleNamespace:
        output = Path(command[command.index("--output-dir") + 1])
        output.mkdir(parents=True, exist_ok=True)
        (output / "run-summary.json").write_text(
            json.dumps(complete_summary(200)), encoding="utf-8"
        )
        return SimpleNamespace(returncode=0)

    monkeypatch.setattr(campaign.subprocess, "run", fake_run)
    result = campaign.run_once(
        data,
        results,
        "baseline",
        "matrix",
        "qps-200",
        200,
        1,
        0,
        1,
        os.getpid(),
        resume=True,
    )
    assert result["evaluation"]["verdict"] == "PASS"
    archived = list(results.glob("incomplete/matrix/baseline/qps-200/rep-1-*"))
    assert len(archived) == 1
    assert (archived[0] / "partial.log").is_file()


def test_capacity_search_and_qps_resolution(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    results = tmp_path / "results"

    def fake_once(*args: object, **kwargs: object) -> dict[str, object]:
        qps = int(args[5])
        verdict = "PASS" if qps <= 200 else "FAIL"
        return {"evaluation": {"verdict": verdict}}

    monkeypatch.setattr(campaign, "run_once", fake_once)
    capacity_result = campaign.capacity(data, results, "baseline", os.getpid())
    assert capacity_result["search_method"] == "automatic-pretest-80pct-binary"
    assert capacity_result["search_start_ratio"] == 0.8
    assert capacity_result["pretest"]["estimate_qps"] == 300
    assert capacity_result["search_start_qps"] == 200
    assert capacity_result["maximum_sustainable_qps"] == 200
    assert capacity_result["first_failed_qps"] == 250
    assert capacity_result["boundary_confirmed"] is True
    assert len(capacity_result["confirmation"]["200"]) == 3
    aggregate_report.write_capacity(results)
    capacity_report = (results / "capacity-report.md").read_text(encoding="utf-8")
    assert "预试验估算 QPS" in capacity_report
    assert "| baseline | 300 | 200 |" in capacity_report
    assert "不会逐个 QPS 降档" in capacity_report
    assert "RSS 长期斜率由四小时长稳测试判定" in capacity_report
    (results / "capacity-optimized.json").write_text(
        json.dumps({"maximum_sustainable_qps": 1000, "first_failed_qps": 1100}),
        encoding="utf-8",
    )
    (results / "capacity-baseline.json").write_text(
        json.dumps({"maximum_sustainable_qps": 1000, "first_failed_qps": 1200}),
        encoding="utf-8",
    )
    matrix, soak, overload = campaign.resolved_qps(data, results)
    assert matrix == [200, 400, 600, 800, 1000]
    assert soak == 800
    assert overload == 1100


def test_capacity_pretest_steps_down_before_binary_search(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    results = tmp_path / "results"

    def fake_once(*args: object, **kwargs: object) -> dict[str, object]:
        qps = int(args[5])
        verdict = "PASS" if qps <= 50 else "FAIL"
        return {"evaluation": {"verdict": verdict}}

    monkeypatch.setattr(campaign, "run_once", fake_once)
    result = campaign.capacity(data, results, "baseline", os.getpid())
    assert result["pretest"]["probes"] == {"100": "FAIL", "50": "PASS"}
    assert result["pretest"]["estimate_qps"] == 75
    assert result["search_start_qps"] == 50
    assert result["maximum_sustainable_qps"] == 50
    assert result["first_failed_qps"] == 100


def test_capacity_restarts_tracer_for_every_probe(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    observed_pids: list[int] = []
    process_starts = 0

    @contextmanager
    def fresh_process() -> Iterator[int]:
        nonlocal process_starts
        process_starts += 1
        yield 10_000 + process_starts

    def fake_once(*args: object, **kwargs: object) -> dict[str, object]:
        qps = int(args[5])
        observed_pids.append(int(args[9]))
        verdict = "PASS" if qps <= 50 else "FAIL"
        return {"evaluation": {"verdict": verdict}}

    monkeypatch.setattr(campaign, "run_once", fake_once)
    result = campaign.capacity(
        data,
        tmp_path / "results",
        "baseline",
        None,
        process_factory=fresh_process,
    )
    assert result["maximum_sustainable_qps"] == 50
    assert process_starts == len(observed_pids)
    assert len(set(observed_pids)) == len(observed_pids)


def test_capacity_stops_when_the_minimum_search_level_fails(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    data["capacity"]["qps_start"] = 10
    data["capacity"]["qps_resolution"] = 1
    probes: list[int] = []

    def fake_once(*args: object, **kwargs: object) -> dict[str, object]:
        probes.append(int(args[5]))
        return {"evaluation": {"verdict": "FAIL"}}

    monkeypatch.setattr(campaign, "run_once", fake_once)
    results = tmp_path / "results"
    with pytest.raises(RuntimeError, match="minimum.*1 QPS"):
        campaign.capacity(data, results, "baseline", os.getpid())
    report = json.loads((results / "capacity-baseline.json").read_text())
    assert report["maximum_sustainable_qps"] is None
    assert report["confirmation"] == {}
    assert probes == [10, 5, 2, 1, 1]


def test_capacity_binary_search_avoids_resolution_wide_linear_scan(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    data["capacity"]["qps_safety_max"] = 12800
    results = tmp_path / "results"

    def fake_once(*args: object, **kwargs: object) -> dict[str, object]:
        qps = int(args[5])
        verdict = "PASS" if qps <= 5000 else "FAIL"
        return {"evaluation": {"verdict": verdict}}

    monkeypatch.setattr(campaign, "run_once", fake_once)
    result = campaign.capacity(data, results, "baseline", os.getpid())
    assert result["pretest"]["estimate_qps"] == 4800
    assert result["search_start_qps"] == 3800
    assert result["maximum_sustainable_qps"] == 5000
    assert result["first_failed_qps"] == 5050
    assert len(result["probes"]) <= 8


def test_capacity_search_uses_pretest_pass_before_halving(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    data["capacity"]["qps_start"] = 10
    data["capacity"]["qps_resolution"] = 1
    formal_probes: list[int] = []

    def fake_once(*args: object, **kwargs: object) -> dict[str, object]:
        label = str(args[4])
        qps = int(args[5])
        if label.startswith("search-"):
            formal_probes.append(qps)
        verdict = "PASS" if qps <= 40 else "FAIL"
        return {"evaluation": {"verdict": verdict}}

    monkeypatch.setattr(campaign, "run_once", fake_once)
    result = campaign.capacity(data, tmp_path / "results", "baseline", os.getpid())
    assert result["pretest"]["last_passed_qps"] == 40
    assert result["deferred_gates"] == ["rss_slope_mb_per_hour"]
    assert result["search_start_qps"] == 48
    assert formal_probes[:2] == [48, 40]
    assert 24 not in formal_probes
    assert result["maximum_sustainable_qps"] == 40
    assert result["first_failed_qps"] == 41


def test_capacity_defers_only_long_run_rss_slope() -> None:
    rss_only = {
        "evaluation": {
            "verdict": "FAIL",
            "missing": [],
            "failed": ["rss_slope_mb_per_hour"],
        }
    }
    real_missing = {
        "evaluation": {
            "verdict": "INCONCLUSIVE",
            "missing": ["trace_completeness"],
            "failed": [],
        }
    }
    real_failure = {
        "evaluation": {
            "verdict": "FAIL",
            "missing": [],
            "failed": ["throughput_ratio", "rss_slope_mb_per_hour"],
        }
    }
    assert campaign.capacity_verdict(rss_only) == "PASS"
    assert campaign.capacity_verdict(real_missing) == "INCONCLUSIVE"
    assert campaign.capacity_verdict(real_failure) == "FAIL"


def test_capacity_confirmation_uses_binary_search_after_candidate_fails(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    data["capacity"]["qps_start"] = 10
    data["capacity"]["qps_resolution"] = 1
    confirmed_qps: list[int] = []

    def fake_once(*args: object, **kwargs: object) -> dict[str, object]:
        label = str(args[4])
        qps = int(args[5])
        if label.startswith("pretest-"):
            verdict = "PASS" if qps <= 40 else "FAIL"
        elif label.startswith("search-"):
            verdict = "PASS" if qps <= 35 else "FAIL"
        else:
            confirmed_qps.append(qps)
            verdict = "PASS" if qps <= 20 else "FAIL"
        return {"evaluation": {"verdict": verdict}}

    monkeypatch.setattr(campaign, "run_once", fake_once)
    result = campaign.capacity(data, tmp_path / "results", "baseline", os.getpid())
    distinct_confirmations = list(dict.fromkeys(confirmed_qps))
    assert distinct_confirmations[:3] == [35, 30, 20]
    assert 36 not in distinct_confirmations
    assert 34 not in distinct_confirmations
    assert len(distinct_confirmations) <= 7
    assert result["confirmation_search_method"] == "adaptive-binary"
    assert result["maximum_sustainable_qps"] == 20
    assert result["first_failed_qps"] == 21


def test_aggregate_rows_deltas_and_partial_reports(tmp_path: Path) -> None:
    items = []
    for version, rss in (("baseline", 50.0), ("optimized", 40.0)):
        for repetition in range(1, 4):
            summary = complete_summary()
            summary["resources"]["rss_mb"]["avg"] = rss
            run = {
                "scenario": "matrix",
                "qps": 100,
                "version": version,
                "repetition": repetition,
                "evaluation": {"verdict": "PASS"},
                "summary": summary,
            }
            items.append(
                (tmp_path / f"{version}-{repetition}" / "run-result.json", run)
            )
    rows = aggregate_report.matrix_rows(items)
    optimized = next(row for row in rows if row["version"] == "optimized")
    assert optimized["rss_avg_mb_delta_pct"] == -20
    aggregate_report.write_performance(tmp_path, rows)
    capacities = aggregate_report.write_capacity(tmp_path)
    aggregate_report.write_soak(tmp_path, items)
    aggregate_report.write_recovery(
        tmp_path,
        items,
        {"tolerance_ratio": 0.1, "recovery_window_seconds": 1},
        campaign_data(tmp_path)["thresholds"],
    )
    aggregate_report.write_fault(
        tmp_path,
        items,
        campaign_data(tmp_path)["fault"],
        campaign_data(tmp_path)["thresholds"],
    )
    regression = aggregate_report.write_regression(tmp_path)
    (tmp_path / "manifest.json").write_text(
        json.dumps({"host": {}, "tools": {"h2load": None}}), encoding="utf-8"
    )
    aggregate_report.write_final(
        tmp_path,
        campaign_data(tmp_path),
        rows,
        capacities,
        items,
        {},
        {},
        regression,
    )
    assert "INCONCLUSIVE" in (tmp_path / "final-report.md").read_text(encoding="utf-8")
    assert json.loads((tmp_path / "final-summary.json").read_text())["verdict"] == (
        "INCONCLUSIVE"
    )
    assert (tmp_path / "performance-comparison.csv").exists()


def test_final_report_summarizes_headline_improvements(tmp_path: Path) -> None:
    items = []
    expected = {
        "baseline": {
            "cpu": 40.0,
            "rss": 100.0,
            "p99": 100.0,
            "trace": 0.999,
            "drop": 0.001,
            "slope": 4.0,
        },
        "optimized": {
            "cpu": 30.0,
            "rss": 80.0,
            "p99": 75.0,
            "trace": 0.9995,
            "drop": 0.0005,
            "slope": 2.0,
        },
    }
    for version, values in expected.items():
        for repetition in range(1, 4):
            summary = complete_summary(100)
            summary["resources"]["cpu_pct"]["avg"] = values["cpu"]
            summary["resources"]["rss_mb"]["avg"] = values["rss"]
            summary["latency_ms"]["p99"] = values["p99"]
            summary["trace_completeness"] = values["trace"]
            summary["drop_rate"] = values["drop"]
            items.append(
                (
                    tmp_path / version / str(repetition) / "run-result.json",
                    {
                        "scenario": "matrix",
                        "qps": 100,
                        "version": version,
                        "repetition": repetition,
                        "evaluation": {"verdict": "PASS"},
                        "summary": summary,
                    },
                )
            )
        soak = complete_summary(80)
        soak["resources"]["rss_mb"]["slope_per_hour"] = values["slope"]
        items.append(
            (
                tmp_path / version / "soak" / "run-result.json",
                {
                    "scenario": "soak",
                    "version": version,
                    "duration_seconds": 14400,
                    "evaluation": {"verdict": "PASS"},
                    "summary": soak,
                },
            )
        )
    rows = aggregate_report.matrix_rows(items)
    capacities = {
        "baseline": {
            "maximum_sustainable_qps": 800,
            "first_failed_qps": 850,
            "boundary_confirmed": True,
        },
        "optimized": {
            "maximum_sustainable_qps": 1200,
            "first_failed_qps": 1250,
            "boundary_confirmed": True,
        },
    }
    manifest = {
        "frozen": {
            "versions": {
                version: {
                    "commit": version,
                    "binary_sha256": f"{version}-binary",
                    "config_sha256": f"{version}-config",
                }
                for version in ("baseline", "optimized")
            }
        },
        "host": {
            "node": "test-host",
            "machine": "x86_64",
            "kernel": "test-kernel",
            "memory_total": "32 GB",
            "cpu_governor": "performance",
            "btf_available": True,
        },
        "tools": {"k6": "k6 test", "openssl": "OpenSSL test", "h2load": None},
    }
    (tmp_path / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
    result = aggregate_report.write_final(
        tmp_path,
        campaign_data(tmp_path),
        rows,
        capacities,
        items,
        {},
        {},
        {"full": True, "checks": [{"exit_code": 0}]},
    )
    comparison = result["comparison"]
    assert comparison["highest_common_qps"] == 100
    assert comparison["metrics"]["maximum_sustainable_qps"]["delta_pct"] == 50
    assert comparison["metrics"]["cpu_avg_pct"]["delta_pct"] == -25
    assert comparison["metrics"]["rss_avg_mb"]["delta_pct"] == -20
    report = (tmp_path / "final-report.md").read_text(encoding="utf-8")
    assert "AgentSight baseline/optimized 全量测试报告" in report
    assert "CPU 占用降低" in report
    assert "内存占用降低" in report
    assert "+50.00%" in report
    assert "-25.00%" in report
    assert "x86_64" in report

    manifest["frozen"]["comparison_mode"] = "aa_calibration"
    (tmp_path / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
    calibration = aggregate_report.write_final(
        tmp_path,
        campaign_data(tmp_path),
        rows,
        capacities,
        items,
        {},
        {},
        {"full": True, "checks": [{"exit_code": 0}]},
    )
    calibration_report = (tmp_path / "final-report.md").read_text(encoding="utf-8")
    assert "AgentSight A/A 全量校准报告" in calibration_report
    assert "A/A 测量波动" in calibration_report
    assert "不得用于声明优化效果" in calibration_report
    assert calibration["comparison_mode"] == "aa_calibration"
    assert calibration["optimization_claim_allowed"] is False


def test_recovery_time_requires_continuous_window() -> None:
    samples = [(0, 120), (1, 105), (2, 104), (3, 103)]
    assert (
        campaign_evidence.continuous_recovery(
            samples, 100, 0.1, 2, higher_is_better=False
        )
        == 1
    )
    assert (
        campaign_evidence.continuous_recovery(
            samples, None, 0.1, 2, higher_is_better=False
        )
        is None
    )


def test_regression_runner_records_command_result(tmp_path: Path) -> None:
    checks: list[dict[str, object]] = []
    status = run_regression.run_check(
        ["/bin/true"], tmp_path, tmp_path / "true.log", checks
    )
    assert status == 0
    run_regression.write_report(tmp_path / "regression.json", checks)
    assert (read_json := json.loads((tmp_path / "regression.json").read_text()))
    assert read_json["checks"][0]["exit_code"] == 0
    assert read_json["full"] is False
    assert read_json["cargo_jobs"] is None


def test_regression_progress_reports_coverage_and_log_path(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    coverage_log = tmp_path / "coverage-report.log"
    coverage_log.write_text("TOTAL 100 5 95%\n", encoding="utf-8")
    run_regression.report_progress("coverage-report", 0, coverage_log)
    output = capsys.readouterr().out
    assert "[OK] coverage-report" in output
    assert str(coverage_log) in output
    assert "coverage: TOTAL 100 5 95%" in output


def test_regression_diff_includes_untracked_python_without_staging(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    responses = iter(
        [
            SimpleNamespace(returncode=0, stdout=b"tracked\n", stderr=b""),
            SimpleNamespace(
                returncode=0,
                stdout=b"src/agentsight/scripts/benchmark/new.py\0ignored.txt\0",
                stderr=b"",
            ),
            SimpleNamespace(returncode=1, stdout=b"untracked\n", stderr=b""),
        ]
    )
    monkeypatch.setattr(
        run_regression.subprocess, "run", lambda *_, **__: next(responses)
    )
    output = tmp_path / "benchmark.diff"
    run_regression.write_benchmark_diff("origin/main", output)
    assert output.read_bytes() == b"tracked\nuntracked\n"


def test_regression_diff_reports_invalid_git_ref(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(
        run_regression.subprocess,
        "run",
        lambda *_, **__: SimpleNamespace(
            returncode=128, stdout=b"", stderr=b"bad revision"
        ),
    )
    with pytest.raises(RuntimeError, match="bad revision"):
        run_regression.write_benchmark_diff("missing", tmp_path / "benchmark.diff")


def test_collect_metrics_cli_and_stop(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    output = tmp_path / "metrics.csv"
    collect_metrics.STOP = False
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "collect_metrics.py",
            "--pid",
            str(os.getpid()),
            "--output",
            str(output),
            "--interval",
            "0.001",
            "--duration",
            "0.004",
            "--input-qps",
            "12",
        ],
    )
    assert collect_metrics.main() == 0
    assert len(output.read_text(encoding="utf-8").splitlines()) >= 2
    collect_metrics.stop(0, None)
    assert collect_metrics.STOP is True
    collect_metrics.STOP = False

    monkeypatch.setattr(
        sys,
        "argv",
        [
            "collect_metrics.py",
            "--pid",
            str(os.getpid()),
            "--output",
            str(output),
            "--interval",
            "0",
        ],
    )
    with pytest.raises(SystemExit, match="interval must be positive"):
        collect_metrics.main()


def test_collect_metrics_reports_each_resource_guard(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(
        collect_metrics, "allocated_size", lambda _: 29 * collect_metrics.GIB
    )
    monkeypatch.setattr(
        collect_metrics.shutil,
        "disk_usage",
        lambda _: SimpleNamespace(free=40 * collect_metrics.GIB),
    )
    monkeypatch.setattr(
        collect_metrics, "available_memory_bytes", lambda: 8 * collect_metrics.GIB
    )
    arguments = (tmp_path, 100.0, 30, 5, 2048, 1536)
    assert collect_metrics.safety_violation(*arguments)["reason"] == "results_budget"

    monkeypatch.setattr(collect_metrics, "allocated_size", lambda _: 0)
    monkeypatch.setattr(
        collect_metrics.shutil,
        "disk_usage",
        lambda _: SimpleNamespace(free=4 * collect_metrics.GIB),
    )
    assert collect_metrics.safety_violation(*arguments)["reason"] == "free_disk"

    monkeypatch.setattr(
        collect_metrics.shutil,
        "disk_usage",
        lambda _: SimpleNamespace(free=40 * collect_metrics.GIB),
    )
    monkeypatch.setattr(
        collect_metrics, "available_memory_bytes", lambda: 1024 * collect_metrics.MIB
    )
    assert collect_metrics.safety_violation(*arguments)["reason"] == "available_memory"

    monkeypatch.setattr(
        collect_metrics, "available_memory_bytes", lambda: 8 * collect_metrics.GIB
    )
    high_rss_arguments = (tmp_path, 1600.0, 30, 5, 2048, 1536)
    assert (
        collect_metrics.safety_violation(*high_rss_arguments)["reason"]
        == "agentsight_rss"
    )


def test_collect_metrics_guard_stops_the_owned_load_process(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    load = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
    output = tmp_path / "metrics.csv"
    safety_output = tmp_path / "safety-stop.json"
    collect_metrics.STOP = False
    monkeypatch.setattr(
        collect_metrics,
        "safety_violation",
        lambda *_: {"schema_version": 1, "reason": "available_memory"},
    )
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "collect_metrics.py",
            "--pid",
            str(os.getpid()),
            "--output",
            str(output),
            "--interval",
            "0.001",
            "--duration",
            "1",
            "--load-pid",
            str(load.pid),
            "--results-root",
            str(tmp_path),
            "--safety-output",
            str(safety_output),
        ],
    )
    try:
        assert collect_metrics.main() == collect_metrics.SAFETY_EXIT_CODE
        load.wait(timeout=5)
    finally:
        if load.poll() is None:
            load.kill()
            load.wait(timeout=5)
        collect_metrics.STOP = False
    assert json.loads(safety_output.read_text(encoding="utf-8"))["reason"] == (
        "available_memory"
    )


def test_collect_metrics_checks_storage_at_low_frequency(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    output = tmp_path / "metrics.csv"
    safety_output = tmp_path / "safety-stop.json"
    storage_checks: list[bool] = []

    def record_check(*args: object) -> None:
        storage_checks.append(bool(args[-1]))
        return None

    collect_metrics.STOP = False
    monkeypatch.setattr(collect_metrics, "safety_violation", record_check)
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "collect_metrics.py",
            "--pid",
            str(os.getpid()),
            "--output",
            str(output),
            "--interval",
            "0.001",
            "--duration",
            "0.004",
            "--load-pid",
            str(os.getpid()),
            "--results-root",
            str(tmp_path),
            "--safety-output",
            str(safety_output),
        ],
    )

    assert collect_metrics.main() == 0
    assert storage_checks[0] is True
    assert storage_checks.count(True) == 1
    assert False in storage_checks
    collect_metrics.STOP = False


def test_fault_injector_main_writes_failure_safe_report(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    output = tmp_path / "fault.json"
    monkeypatch.setattr(
        fault_injector,
        "inject",
        lambda *args: {
            "server_healthy_after": True,
            "process_alive_after": True,
            "outcomes": {},
        },
    )
    monkeypatch.setattr(
        sys,
        "argv",
        ["fault_injector.py", "--count", "1", "--output", str(output)],
    )
    assert fault_injector.main() == 0
    assert json.loads(output.read_text())["server_healthy_after"] is True
    monkeypatch.setattr(
        sys,
        "argv",
        ["fault_injector.py", "--count", "0", "--output", str(output)],
    )
    with pytest.raises(SystemExit, match="must be positive"):
        fault_injector.main()


def test_mock_server_main_and_argument_validation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    closed = []

    class FakeServer:
        def __init__(self, *_: object) -> None:
            self.socket = object()

        def serve_forever(self) -> None:
            raise KeyboardInterrupt

        def server_close(self) -> None:
            closed.append(True)

    class FakeContext:
        def load_cert_chain(self, *_: object) -> None:
            pass

        def set_alpn_protocols(self, _: object) -> None:
            pass

        def wrap_socket(self, *_: object, **__: object) -> object:
            return object()

    monkeypatch.setattr(mock_llm_server, "BenchmarkHTTPServer", FakeServer)
    monkeypatch.setattr(mock_llm_server.ssl, "SSLContext", lambda _: FakeContext())
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "mock_llm_server.py",
            "--cert",
            str(tmp_path / "c"),
            "--key",
            str(tmp_path / "k"),
        ],
    )
    assert mock_llm_server.main() == 0
    assert closed
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "mock_llm_server.py",
            "--cert",
            "c",
            "--key",
            "k",
            "--chunks",
            "0",
        ],
    )
    with pytest.raises(SystemExit, match="must be non-negative"):
        mock_llm_server.main()


def test_validate_results_cli_writes_report(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    load = tmp_path / "load.jsonl"
    load.write_text(
        json.dumps(
            {
                "metric": "benchmark_requests",
                "data": {"value": 1, "tags": {}},
            }
        )
        + "\n",
        encoding="utf-8",
    )
    request_log = tmp_path / "requests.jsonl"
    request_log.write_text(
        json.dumps({"request_id": "bench-cli", "http_status": 200}) + "\n",
        encoding="utf-8",
    )
    database = tmp_path / "events.db"
    with sqlite3.connect(database) as connection:
        connection.execute(
            "CREATE TABLE token_records (request_id TEXT, input_tokens INTEGER, output_tokens INTEGER)"
        )
        connection.execute("INSERT INTO token_records VALUES ('bench-cli', 12, 8)")
    output = tmp_path / "validation.json"
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "validate_results.py",
            "--load-results",
            str(load),
            "--request-log",
            str(request_log),
            "--db",
            str(database),
            "--expected-total-tokens",
            "20",
            "--output",
            str(output),
        ],
    )
    assert validate_results.main() == 0
    assert json.loads(output.read_text())["completeness_ratio"] == 1
    terminal_output = capsys.readouterr().out
    assert '"missing_count": 0' in terminal_output
    assert '"missing_ids"' not in terminal_output


def test_campaign_runs_every_non_capacity_scenario(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    results = tmp_path / "results"
    calls: list[tuple[object, ...]] = []

    def fake_once(*args: object, **kwargs: object) -> dict[str, object]:
        calls.append((*args, kwargs))
        return {"evaluation": {"verdict": "PASS"}}

    monkeypatch.setattr(campaign, "run_once", fake_once)
    campaign.run_scenario(data, results, "baseline", "smoke", os.getpid())
    for version, maximum, failed in (
        ("baseline", 1000, 1100),
        ("optimized", 1200, 1300),
    ):
        (results / f"capacity-{version}.json").parent.mkdir(parents=True, exist_ok=True)
        (results / f"capacity-{version}.json").write_text(
            json.dumps(
                {"maximum_sustainable_qps": maximum, "first_failed_qps": failed}
            ),
            encoding="utf-8",
        )
    for scenario in ("matrix", "soak", "recovery", "fault"):
        campaign.run_scenario(data, results, "baseline", scenario, os.getpid())
    assert len(calls) == 27
    assert calls[-1][-1] == {"faults": 1, "resume": False}


def test_campaign_main_routes_validate_and_capacity(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    config = tmp_path / "campaign.json"
    config.write_text(json.dumps(data), encoding="utf-8")
    results = tmp_path / "results"
    frozen = []
    capacities = []
    monkeypatch.setattr(campaign, "ensure_manifest", lambda *args: frozen.append(args))
    monkeypatch.setattr(campaign, "capacity", lambda *args: capacities.append(args))
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "campaign.py",
            "--campaign",
            str(config),
            "--results",
            str(results),
            "--scenario",
            "validate",
        ],
    )
    assert campaign.main() == 0
    assert frozen
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "campaign.py",
            "--campaign",
            str(config),
            "--results",
            str(results),
            "--scenario",
            "capacity",
            "--version",
            "baseline",
            "--pid",
            str(os.getpid()),
        ],
    )
    assert campaign.main() == 0
    assert capacities


def test_manifest_collects_real_environment_and_rejects_bad_config(
    tmp_path: Path,
) -> None:
    data = campaign_data(tmp_path)
    config_path = tmp_path / "campaign.json"
    config_path.write_text(json.dumps(data), encoding="utf-8")
    manifest = campaign_manifest.environment_manifest(config_path, data)
    assert manifest["host"]["machine"]
    assert manifest["tools"]["python"]
    assert campaign_manifest.command_version(["command-that-does-not-exist"]) is None
    bad_config = tmp_path / "bad.json"
    bad_config.write_text("{}", encoding="utf-8")
    assert campaign_manifest.observes_mock_server(bad_config) is False
    data["versions"]["baseline"]["config"] = str(bad_config)
    with pytest.raises(ValueError, match="must discover"):
        campaign_manifest.frozen_inputs(config_path, data)


def test_full_scenario_reports_use_raw_artifacts(tmp_path: Path) -> None:
    items: list[tuple[Path, dict[str, object]]] = []
    for version in ("baseline", "optimized"):
        soak_summary = complete_summary(800)
        soak_summary["resources"].update(
            {
                "file_descriptors": {"slope_per_hour": 0.1},
                "threads": {"slope_per_hour": 0.0},
                "active_connections": {"slope_per_hour": 0.1},
            }
        )
        items.append(
            (
                tmp_path / version / "soak" / "run-result.json",
                {
                    "scenario": "soak",
                    "version": version,
                    "duration_seconds": 14400,
                    "evaluation": {"verdict": "PASS"},
                    "summary": soak_summary,
                },
            )
        )
    for label, rss in (("stable", 100.0), ("overload", 130.0), ("recover", 105.0)):
        path = tmp_path / "baseline" / label / "run-result.json"
        summary = complete_summary(800)
        summary["resources"]["rss_mb"].update(
            {"avg": rss, "first": rss, "last": rss, "max": rss}
        )
        run = {
            "scenario": "recovery",
            "label": label,
            "version": "baseline",
            "repetition": 1,
            "started_at_unix": 0,
            "summary": summary,
        }
        items.append((path, run))
        if label == "recover":
            measurement = path.parent / "measurement"
            measurement.mkdir(parents=True)
            (measurement / "metrics.csv").write_text(
                "timestamp,rss_mb\n0,120\n1,105\n2,104\n", encoding="utf-8"
            )
    fault_path = tmp_path / "optimized" / "fault" / "run-result.json"
    fault_measurement = fault_path.parent / "measurement"
    fault_measurement.mkdir(parents=True)
    (fault_measurement / "fault-results.json").write_text(
        json.dumps(
            {
                "outcomes": {"invalid_json": {"HTTP/1.1 200 OK": 2}},
                "server_healthy_after": True,
                "process_alive_after": True,
            }
        ),
        encoding="utf-8",
    )
    items.append(
        (
            fault_path,
            {
                "scenario": "fault",
                "version": "optimized",
                "summary": complete_summary(),
            },
        )
    )
    aggregate_report.write_soak(tmp_path, items)
    aggregate_report.write_recovery(
        tmp_path,
        items,
        {"tolerance_ratio": 0.1, "recovery_window_seconds": 1},
        campaign_data(tmp_path)["thresholds"],
    )
    aggregate_report.write_fault(
        tmp_path,
        items,
        campaign_data(tmp_path)["fault"],
        campaign_data(tmp_path)["thresholds"],
    )
    assert "PASS" in (tmp_path / "soak-report.md").read_text()
    assert "invalid_json" in (tmp_path / "fault-report.md").read_text()
    assert "1.00s" in (tmp_path / "recovery-report.md").read_text()


def test_regression_main_builds_targeted_and_full_commands(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    seen: list[list[str]] = []

    def fake_check(
        command: list[str], cwd: Path, log_path: Path, checks: list[dict[str, object]]
    ) -> int:
        seen.append(command)
        checks.append(
            {
                "command": " ".join(command),
                "cwd": str(cwd),
                "log": log_path.name,
                "duration_seconds": 0,
                "exit_code": 0,
            }
        )
        return 0

    monkeypatch.setattr(run_regression, "run_check", fake_check)
    monkeypatch.setattr(run_regression, "missing_python_modules", list)
    monkeypatch.setattr(
        run_regression,
        "write_benchmark_diff",
        lambda _branch, path: path.write_text("", encoding="utf-8"),
    )
    monkeypatch.setattr(
        sys, "argv", ["run_regression.py", "--results", str(tmp_path / "targeted")]
    )
    assert run_regression.main() == 0
    assert len(seen) == 8
    assert all("uv" not in command for command in seen)
    assert seen[0][0] == sys.executable
    assert "diff_cover.diff_cover_tool" in seen[3]
    assert all(
        command[:4] == ["rustup", "run", "1.89.0", "cargo"] for command in seen[4:]
    )
    assert all("--jobs" in command and "2" in command for command in seen[4:])
    seen.clear()
    monkeypatch.setattr(
        sys,
        "argv",
        ["run_regression.py", "--results", str(tmp_path / "full"), "--full"],
    )
    assert run_regression.main() == 0
    assert len(seen) == 7
    clippy = next(command for command in seen if "clippy" in command)
    workspace_test = next(command for command in seen if "test" in command)
    for package in run_regression.CI_EXCLUDED_PACKAGES:
        assert package in clippy
        assert package in workspace_test
    assert workspace_test[workspace_test.index("--jobs") + 1] == "2"
    assert workspace_test[workspace_test.index("--test-threads") + 1] == "2"

    seen.clear()
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "run_regression.py",
            "--results",
            str(tmp_path / "single-job"),
            "--full",
            "--cargo-jobs",
            "1",
        ],
    )
    assert run_regression.main() == 0
    workspace_test = next(command for command in seen if "test" in command)
    assert workspace_test[workspace_test.index("--jobs") + 1] == "1"
    report = json.loads((tmp_path / "single-job/regression.json").read_text())
    assert report["cargo_jobs"] == 1
