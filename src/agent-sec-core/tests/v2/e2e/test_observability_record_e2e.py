"""OBS-001..006 / DPROC-022: real V2 ingestion and durable storage acceptance.

Uses V2 binaries on PATH. SQLite reads are test assertions, not a product query API.
"""

import concurrent.futures
import json
import shutil
import sqlite3
import subprocess
import tempfile
from contextlib import closing
from datetime import datetime, timezone
from pathlib import Path

import pytest
from tests.v2.e2e.test_otel_e2e import OtelEnvironment

ROOT = Path(__file__).resolve().parents[3]
CASES = json.loads((ROOT / "v2/fixtures/observability/v1-records.json").read_text())


@pytest.fixture
def ingestion():
    binaries = {
        name: shutil.which(name) for name in ("agent-sec-cli", "agent-sec-daemon")
    }
    assert all(binaries.values()), "build V2 binaries and place them on PATH"
    # RuntimeLease requires an owner-only directory, independent of ambient TMPDIR.
    with tempfile.TemporaryDirectory(prefix="asc-obs-", dir="/tmp") as directory:
        environment = OtelEnvironment(Path(directory), binaries)
        environment.env["RUST_LOG"] = "off"
        try:
            yield environment
        finally:
            environment.close()


def cli(env, payload, *options):
    return subprocess.run(
        [
            env.binaries["agent-sec-cli"],
            *options,
            "--socket",
            str(env.socket),
            "observability",
            "record",
            "--stdin",
        ],
        input=json.dumps(payload),
        text=True,
        capture_output=True,
        env=env.env,
        timeout=10,
        check=False,
    )


def rows(env):
    path = env.directory / "data/observability.db"
    with closing(sqlite3.connect(f"file:{path}?mode=ro", uri=True)) as connection:
        return connection.execute(
            "SELECT hook, observed_at, metadata_json, metrics_json FROM observability_events ORDER BY id"
        ).fetchall()


def persisted(env):
    return [
        {
            "hook": hook,
            "observedAt": timestamp,
            "metadata": json.loads(metadata),
            "metrics": json.loads(metrics),
        }
        for hook, timestamp, metadata, metrics in rows(env)
    ]


def raw_request(session="s", tool=True):
    return {
        "method": "obs.record",
        "params": {
            "hook": "before_tool_call",
            "observedAt": "2030-01-02T03:04:05Z",
            "metrics": {"tool_name": "read_file"},
        },
        "traceContext": {
            "version": 1,
            "baggage": f"agentsec.session.id={session},agentsec.run.id=r"
            + (",agentsec.tool_call.id=t" if tool else ""),
        },
    }


def test_v1_cli_records_persist_and_survive_restart(ingestion):
    ingestion.env["RUST_LOG"] = "info"
    ingestion.start(authorize=False)
    for case in CASES:
        result = cli(
            ingestion,
            case["input"],
            "--trace-context",
            json.dumps(
                {
                    "sessionId": "stale",
                    "runId": "stale",
                    "callId": "stale",
                    "toolCallId": "stale",
                    "agentName": "codex",
                }
            ),
            "--otel-context",
            json.dumps(
                {
                    "version": 1,
                    "traceparent": "00-11111111111111111111111111111111-2222222222222222-00",
                }
            ),
        )
        assert result.returncode == 0, result.stderr
        assert result.stdout == ""
    contexts = [
        json.loads(item["fields"]["correlation"])
        for item in ingestion.records()
        if item.get("fields", {}).get("reason") == "request_started"
    ]
    assert len(contexts) == len(CASES)
    assert all(
        item["trace_id"] == "11111111111111111111111111111111" for item in contexts
    )
    assert all(
        item["agent"]["session_id"] == CASES[0]["expected"]["metadata"]["sessionId"]
        for item in contexts
    )
    assert all(item["agent"]["agent_name"] == "codex" for item in contexts)
    assert "fixture-input" not in (ingestion.directory / "daemon.log").read_text()
    expected = [case["expected"] for case in CASES]
    assert persisted(ingestion) == expected
    log = ingestion.directory / "data/observability.jsonl"
    assert [json.loads(line) for line in log.read_text().splitlines()] == expected
    assert log.stat().st_mode & 0o777 == 0o600
    assert (ingestion.directory / "data").stat().st_mode & 0o777 == 0o700
    ingestion.stop()
    ingestion.log.close()
    ingestion.start(authorize=False)
    assert persisted(ingestion) == expected
    assert ingestion.call(raw_request())["result"] == {}
    assert len(rows(ingestion)) == len(expected) + 1


def test_daemon_rejects_invalid_params_and_missing_metadata_without_writes(ingestion):
    ingestion.start(authorize=False)
    requests = []
    request = raw_request()
    request.pop("traceContext")
    requests.append(request)
    requests.append(raw_request(tool=False))
    for key, value in [
        ("metadata", {"sessionId": "spoof"}),
        ("hook", "unsupported"),
        ("metrics", {"unknown": 1}),
        ("observedAt", "2030-01-02T03:04:05"),
    ]:
        request = raw_request()
        request["params"][key] = value
        requests.append(request)
    request = raw_request()
    request["traceContext"]["baggage"] += ",agentsec.session.id=duplicate"
    requests.append(request)
    for request in requests:
        response = ingestion.call(request)
        assert response["error"]["code"] == "invalid_argument", response
    assert not (ingestion.directory / "data/observability.jsonl").exists()
    assert not (ingestion.directory / "data/observability.db").exists()


def test_concurrent_ingestion_isolates_baggage_and_no_carrier_does_not_inherit(
    ingestion,
):
    ingestion.start(authorize=False)
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        responses = list(
            pool.map(lambda i: ingestion.call(raw_request(f"s-{i}")), range(24))
        )
    assert all(response.get("result") == {} for response in responses), responses
    stored = persisted(ingestion)
    assert {record["metadata"]["sessionId"] for record in stored} == {
        f"s-{i}" for i in range(24)
    }
    assert len(stored) == 24
    request = raw_request()
    request.pop("traceContext")
    assert ingestion.call(request)["error"]["code"] == "invalid_argument"
    assert len(rows(ingestion)) == 24


@pytest.mark.parametrize("broken", ["observability.jsonl", "observability.db"])
def test_write_failures_surface_without_retry_or_rollback(ingestion, broken):
    ingestion.start(authorize=False)
    (ingestion.directory / "data" / broken).mkdir()
    result = cli(ingestion, CASES[0]["input"])
    assert result.returncode == 1
    assert result.stdout == ""
    assert "failed to write observability record" in result.stderr
    log = ingestion.directory / "data/observability.jsonl"
    if broken.endswith(".db"):
        assert len(log.read_text().splitlines()) == 1
    else:
        assert not (ingestion.directory / "data/observability.db").exists()


def test_cli_input_errors_and_unavailable_daemon_never_fall_back(ingestion):
    ingestion.socket = ingestion.directory / "absent.sock"
    for payload in [
        [],
        {},
        CASES[0]["input"] | {"metadata": {}},
        CASES[0]["input"] | {"metrics": {"unknown": 1}},
    ]:
        result = cli(ingestion, payload)
        assert result.returncode == 1
        assert result.stdout == ""
        assert "Error:" in result.stderr
    result = cli(ingestion, CASES[0]["input"])
    assert result.returncode == 1
    assert result.stdout == ""
    assert not (ingestion.directory / "data").exists()


def test_cli_normalizes_numeric_timestamp_for_the_daemon_contract(ingestion):
    ingestion.start(authorize=False)
    payload = CASES[0]["input"] | {"observedAt": 1893542645123.456}
    result = cli(ingestion, payload)
    assert result.returncode == 0, result.stderr
    assert result.stdout == ""
    assert persisted(ingestion)[0]["observedAt"] == "2030-01-02T00:04:05.123456Z"


def run_v1_record_cli(ingestion, *args, input_text):
    """Run the V1 CLI arguments through V2 and preserve the security-event baseline."""
    ingestion.start(authorize=False)
    data_dir = ingestion.directory / "data"
    security_log = data_dir / "security-events.jsonl"
    baseline_log = security_log.read_bytes()
    with closing(
        sqlite3.connect(f"file:{data_dir / 'security-events.db'}?mode=ro", uri=True)
    ) as connection:
        baseline_count = connection.execute(
            "SELECT count(*) FROM security_events"
        ).fetchone()[0]
        result = subprocess.run(
            [
                ingestion.binaries["agent-sec-cli"],
                "--socket",
                str(ingestion.socket),
                *args,
            ],
            input=input_text,
            text=True,
            capture_output=True,
            env=ingestion.env,
            timeout=10,
            check=False,
        )
        assert (
            connection.execute("SELECT count(*) FROM security_events").fetchone()[0]
            == baseline_count
        )
    assert security_log.read_bytes() == baseline_log
    return result


# Ported from tests/e2e/cli/test_observability_record_jsonl_e2e.py.
def test_v1_observability_record_json_creates_observability_jsonl(ingestion):
    data_dir = ingestion.directory / "data"
    payload = {
        "hook": "after_tool_call",
        "observedAt": "2026-05-11T12:00:00Z",
        "metadata": {
            "sessionId": "session-e2e",
            "runId": "run-e2e",
            "toolCallId": "tool-call-e2e",
        },
        "metrics": {
            "result": {"ok": True},
            "duration_ms": 25,
        },
    }

    result = run_v1_record_cli(
        ingestion,
        "observability",
        "record",
        "--format",
        "json",
        "--stdin",
        input_text=json.dumps(payload),
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == ""
    records = [
        json.loads(line)
        for line in (data_dir / "observability.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
    ]
    assert records[0]["hook"] == "after_tool_call"
    assert "schemaVersion" not in records[0]
    assert records[0]["metadata"]["runId"] == "run-e2e"
    assert records[0]["metadata"]["toolCallId"] == "tool-call-e2e"
    assert records[0]["metrics"] == {"result": {"ok": True}, "duration_ms": 25}


# Ported from tests/e2e/cli/test_observability_record_sqlite_e2e.py.
def test_v1_observability_record_json_creates_observability_sqlite_index(ingestion):
    data_dir = ingestion.directory / "data"
    payload = {
        "hook": "after_tool_call",
        "observedAt": datetime.now(timezone.utc).isoformat(),
        "metadata": {
            "sessionId": "session-e2e",
            "runId": "run-e2e",
            "callId": "call-e2e",
            "toolCallId": "tool-call-e2e",
        },
        "metrics": {
            "result": {"ok": True},
            "duration_ms": 25,
            "result_size_bytes": 128,
        },
    }

    result = run_v1_record_cli(
        ingestion,
        "observability",
        "record",
        "--format",
        "json",
        "--stdin",
        input_text=json.dumps(payload),
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == ""
    assert (data_dir / "observability.jsonl").exists()
    assert (data_dir / "observability.db").exists()

    conn = sqlite3.connect(data_dir / "observability.db")
    try:
        row = conn.execute("""
            SELECT hook, session_id, run_id, call_id, tool_call_id, metrics_json
            FROM observability_events
            """).fetchone()
    finally:
        conn.close()

    assert row[0:5] == (
        "after_tool_call",
        "session-e2e",
        "run-e2e",
        "call-e2e",
        "tool-call-e2e",
    )
    assert json.loads(row[5])["result_size_bytes"] == 128
