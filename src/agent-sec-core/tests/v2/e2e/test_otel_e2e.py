"""Real CLI/daemon local tracing and diagnostic conformance.

Put V2 build outputs or RPM-installed binaries on PATH before running.
No Collector or OTLP exporter is used.
"""

import concurrent.futures
import json
import os
import shutil
import signal
import socket
import sqlite3
import subprocess
import time
from contextlib import closing, contextmanager
from pathlib import Path
from urllib.parse import quote

import pytest

TRACE = "11111111111111111111111111111111"
PARENT = "2222222222222222"
CANARY = "DO_NOT_EXPORT_SECRET_PAYLOAD"


class OtelEnvironment:
    """Owns a daemon and its CLI runner for one local correlation test."""

    def __init__(self, directory: Path, binaries: dict[str, str]):
        self.directory = directory
        self.binaries = binaries
        self.env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("OTEL_")
        }
        self.env["RUST_LOG"] = "info"
        self.env["AGENT_SEC_DATA_DIR"] = str(directory / "data")
        self.daemon = None
        self.log = None

    def close(self):
        try:
            if self.daemon is not None:
                self.stop()
        finally:
            if self.log is not None:
                self.log.close()

    def start(self, authorize=True, stderr=None, **env):
        self.socket = self.directory / "daemon.sock"
        self.log = (self.directory / "daemon.log").open("w+")
        command = [self.binaries["agent-sec-daemon"], "--socket", str(self.socket)]
        if authorize:
            command.extend(["--policy-admin-uid", str(os.getuid())])
        self.daemon = subprocess.Popen(
            command,
            env=self.env | env,
            stdout=subprocess.PIPE,
            stderr=self.log if stderr is None else stderr,
        )
        deadline = time.monotonic() + 10
        while not self.socket.exists():
            assert self.daemon.poll() is None
            assert time.monotonic() < deadline
            time.sleep(0.01)

    def stop(self):
        child, self.daemon = self.daemon, None
        child.send_signal(signal.SIGTERM)
        try:
            output, _ = child.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            child.kill()
            child.communicate()
            pytest.fail("daemon shutdown exceeded watchdog")
        assert child.returncode == 0
        assert output == b""

    def call(self, payload):
        if not isinstance(payload, bytes):
            payload = json.dumps(payload).encode()
        with socket.socket(socket.AF_UNIX) as client:
            client.settimeout(6)
            client.connect(str(self.socket))
            client.sendall(payload + b"\n")
            data = bytearray()
            while not data.endswith(b"\n"):
                chunk = client.recv(8192)
                assert chunk
                data.extend(chunk)
        return json.loads(data)

    def cli(self, *args, command=None, **env):
        return subprocess.run(
            [
                self.binaries["agent-sec-cli"],
                *args,
                "--socket",
                str(self.socket),
                *(command or ["policy", "list"]),
            ],
            env=self.env | env,
            capture_output=True,
            timeout=10,
            check=False,
        )

    def records(self):
        self.log.flush()
        return [
            json.loads(line)
            for line in (self.directory / "daemon.log").read_text().splitlines()
            if line.startswith("{")
        ]


@pytest.fixture
def otel(tmp_path_factory):
    """Yields isolated processes and guarantees cleanup, including failed assertions."""
    binaries = {}
    for name in ("agent-sec-cli", "agent-sec-daemon"):
        binary = shutil.which(name)
        if binary is None:
            pytest.fail(f"{name} not found on PATH; install or build the V2 binaries")
        binaries[name] = binary
    # Keep UDS paths short even when the test function has a long name.
    environment = OtelEnvironment(tmp_path_factory.mktemp("otel"), binaries)
    try:
        yield environment
    finally:
        environment.close()


def test_cli_daemon_correlation_and_safe_logs(otel):
    otel.start()
    native = {
        "version": 1,
        "traceparent": f"00-{TRACE}-{PARENT}-01",
        "baggage": "agentsec.session.id=native,unknown=" + CANARY,
    }
    trace_context_input = {
        "sessionId": "caller-session",
        "runId": "run",
        "agentName": "agent",
        "toolCallId": "tool",
        "callId": "call",
        "trace_id": "opaque",
    }
    result = otel.cli(
        "--trace-context",
        json.dumps(trace_context_input),
        "--otel-context",
        json.dumps(native),
        AGENT_SEC_INVOCATION_ID="explicit",
    )
    assert result.returncode == 0, result.stderr
    json.loads(result.stdout)
    otel.call({"method": CANARY, "params": {"secret": CANARY}})
    otel.stop()
    cli_records = [
        json.loads(json.loads(line)["fields"]["correlation"])
        for line in result.stderr.decode().splitlines()
    ]
    assert cli_records
    assert all(record["trace_id"] == TRACE for record in cli_records)
    assert CANARY not in (otel.directory / "daemon.log").read_text()
    records = [
        json.loads(record["fields"]["correlation"])
        for record in otel.records()
        if record["fields"]["reason"] == "request_started"
    ]
    assert records[0]["trace_id"] == TRACE
    assert records[0]["span_id"] != cli_records[-1]["span_id"]
    assert records[0]["agent"] == {
        "session_id": "caller-session",
        "run_id": "run",
        "agent_name": "agent",
        "tool_call_id": "tool",
        "call_id": "call",
    }
    assert records[0]["compatibility"] == {
        "trace_id": "opaque",
        "invocation_label": "explicit",
    }


def test_raw_uds_unsampled_concurrent_roots_and_carrier_isolation(otel):
    otel.start()

    def call(index):
        context = (
            {"version": 1, "baggage": f"agentsec.session.id=s{index}"}
            if index % 2
            else None
        )
        return otel.call({"method": "policy.templates.list", "traceContext": context})

    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        results = list(pool.map(call, range(24)))
    assert all(("result" in result for result in results))
    otel.stop()
    records = [
        json.loads(record["fields"]["correlation"])
        for record in otel.records()
        if record["fields"]["reason"] == "request_started"
    ]
    assert len(records) == 24
    assert len({record["trace_id"] for record in records}) == 24
    for record in records:
        assert record["span_id"] != "0" * 16
        assert record["request_span_id"] == record["span_id"]
    assert {record["agent"]["session_id"] for record in records if record["agent"]} == {
        f"s{i}" for i in range(1, 24, 2)
    }


def test_schema_and_frame_budgets_are_independent(otel):
    otel.start(RUST_LOG="off")
    for carrier in [
        {"version": 2},
        {"version": 1, "baggage": None},
        {"version": 1, "uid": 0},
    ]:
        assert (
            otel.call({"method": "policy.templates.list", "traceContext": carrier})[
                "error"
            ]["code"]
            == "invalid_request"
        )
    assert (
        otel.call(
            b'{"method":"policy.templates.list","traceContext":{"version":1,"baggage":"a","baggage":"b"}}'
        )["error"]["code"]
        == "invalid_request"
    )
    rejected = otel.call(
        {
            "method": "policy.templates.create",
            "params": {
                "policyName": "must-not-exist",
                "template": {"kind": "prevent_file_deletion", "files": ["/example"]},
            },
            "traceContext": {"version": 2},
        }
    )
    assert rejected["error"]["code"] == "invalid_request"
    assert otel.call({"method": "policy.templates.list"})["result"]["items"] == []
    prefix = b'{"method":"unknown","params":{"padding":"'
    suffix = b'"}}'
    base = prefix + b"x" * (4 * 1024 * 1024 - 1 - len(prefix) - len(suffix)) + suffix
    assert otel.call(base)["error"]["code"] == "unknown_method"
    extended = (
        base[:-1] + b',"traceContext":{"version":1,"baggage":"agentsec.session.id=s"}}'
    )
    assert otel.call(extended)["error"]["code"] == "unknown_method"
    assert otel.call(b" " + extended)["error"]["code"] == "invalid_request"
    assert "result" in otel.call(
        {
            "method": "policy.templates.list",
            "traceContext": {"version": 1, "traceparent": "invalid"},
        }
    )


@pytest.mark.skipif(os.getuid() == 0, reason="requires a non-root kernel peer")
def test_baggage_cannot_override_kernel_peer_authorization(otel):
    otel.start(authorize=False, RUST_LOG="off")
    for context in [
        None,
        {"version": 1, "baggage": "uid=0,role=administrator,agentsec.agent.name=root"},
        {"version": 1, "traceparent": "bad"},
    ]:
        result = otel.call({"method": "policy.templates.list", "traceContext": context})
        assert result["error"]["code"] == "permission_denied"


def test_same_trace_requests_keep_distinct_unsampled_log_anchors(otel):
    otel.start()

    def call(index):
        return otel.call(
            {
                "method": "policy.templates.list",
                "traceContext": {
                    "version": 1,
                    "traceparent": f"00-{TRACE}-{PARENT}-00",
                    "baggage": f"agentsec.session.id=s{index},agentsec.run.id=r{index}",
                },
            }
        )

    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        assert all(("result" in result for result in pool.map(call, range(24))))
    otel.stop()
    requests = {}
    for record in otel.records():
        fields = record["fields"]
        correlation = json.loads(fields["correlation"])
        assert correlation["trace_id"] == TRACE
        assert correlation["span_id"] == correlation["request_span_id"]
        requests.setdefault(correlation["request_span_id"], []).append(
            (fields["reason"], correlation)
        )
    assert len(requests) == 24
    sessions = set()
    for records in requests.values():
        assert [reason for reason, _ in records] == [
            "request_started",
            "request_completed",
        ]
        assert records[0][1] == records[1][1]
        sessions.add(records[0][1]["agent"]["session_id"])
    assert sessions == {f"s{index}" for index in range(24)}


def test_maximum_unicode_baggage_with_full_business_frame(otel):
    otel.start()
    value = "🦀" * 256
    keys = ["agent.name", "session.id", "run.id", "call.id", "tool_call.id"]
    baggage = ",".join(f"agentsec.{key}={quote(value, safe='')}" for key in keys)
    assert len(baggage.encode()) <= 16384
    prefix = b'{"method":"unknown","params":{"padding":"'
    suffix = b'"}}'
    base = prefix + b"x" * (4 * 1024 * 1024 - 1 - len(prefix) - len(suffix)) + suffix
    carrier = json.dumps({"version": 1, "baggage": baggage}).encode()
    frame = base[:-1] + b',"traceContext":' + carrier + b"}"
    assert otel.call(frame)["error"]["code"] == "unknown_method"
    assert otel.call(b" " + frame)["error"]["code"] == "invalid_request"
    assert (
        otel.call(
            {
                "method": "policy.templates.list",
                "traceContext": {"version": 1, "baggage": "x" * 32768},
            }
        )["error"]["code"]
        == "invalid_request"
    )
    otel.stop()
    started = [
        json.loads(r["fields"]["correlation"])
        for r in otel.records()
        if r["fields"]["reason"] == "request_started"
    ]
    assert len(started) == 1
    assert len(started[0]["agent"]) == 5
    assert all((field == value for field in started[0]["agent"].values()))


@contextmanager
def blocked_stderr():
    read_fd, write_fd = os.pipe()
    with os.fdopen(read_fd, "rb", buffering=0), os.fdopen(
        write_fd, "wb", buffering=0
    ) as stderr:
        os.set_blocking(stderr.fileno(), False)
        try:
            while True:
                os.write(stderr.fileno(), b"x" * 4096)
        except BlockingIOError:
            pass
        finally:
            # The child shares this file description: restore blocking writes
            # before emitting diagnostics so this is a real stalled sink.
            os.set_blocking(stderr.fileno(), True)
        yield stderr


@pytest.mark.parametrize("log_filter", ["info", "off"])
def test_blocked_stderr_preserves_startup_responses_and_shutdown(otel, log_filter):
    with blocked_stderr() as stderr:
        # Fill before startup: PAP warning and obsolete exporter configuration
        # must not prevent binding the socket even with correlation logging off.
        otel.start(
            stderr=stderr,
            RUST_LOG=log_filter,
            OTEL_TRACES_EXPORTER="otlp",
            OTEL_BSP_MAX_QUEUE_SIZE="invalid",
        )
        assert otel.call(b"")["error"]["code"] == "invalid_request"
        assert (
            otel.call(b" " * (4 * 1024 * 1024 + 32768))["error"]["code"]
            == "resource_exhausted"
        )
        # Saturate the diagnostic queue while checking each business response.
        for _ in range(40):
            assert "result" in otel.call({"method": "policy.templates.list"})
        cli = subprocess.run(
            [
                otel.binaries["agent-sec-cli"],
                "--socket",
                str(otel.socket),
                "policy",
                "list",
            ],
            env=otel.env
            | {"RUST_LOG": log_filter, "OTEL_BSP_MAX_QUEUE_SIZE": "invalid"},
            stdout=subprocess.PIPE,
            stderr=stderr,
            timeout=10,
            check=False,
        )
        assert cli.returncode == 0
        json.loads(cli.stdout)
        scan = subprocess.run(
            [
                otel.binaries["agent-sec-cli"],
                "--socket",
                str(otel.socket),
                "scan-code",
                "--code",
                "echo safe",
            ],
            env=otel.env | {"RUST_LOG": log_filter},
            stdout=subprocess.PIPE,
            stderr=stderr,
            timeout=10,
            check=False,
        )
        assert scan.returncode == 0
        assert json.loads(scan.stdout)["verdict"] == "pass"
        # Startup failure diagnostics also cannot delay an exit. The running
        # daemon owns the socket, so a second daemon must fail without serving.
        duplicate = subprocess.run(
            [otel.binaries["agent-sec-daemon"], "--socket", str(otel.socket)],
            env=otel.env | {"RUST_LOG": log_filter},
            stdout=subprocess.PIPE,
            stderr=stderr,
            timeout=10,
            check=False,
        )
        assert duplicate.returncode == 1
        assert duplicate.stdout == b""
        # Keep the pipe undrained until all processes have exited.
        otel.stop()


@pytest.mark.parametrize("log_filter", ["info", "off"])
@pytest.mark.parametrize("fault", ["jsonl_write", "sqlite_write", "newer_schema"])
def test_storage_fault_diagnostics_do_not_block_scan_or_shutdown(
    otel, log_filter, fault
):
    data = Path(otel.env["AGENT_SEC_DATA_DIR"])
    if fault == "newer_schema":
        data.mkdir(mode=0o700)
        with closing(sqlite3.connect(data / "security-events.db")) as connection:
            connection.execute("PRAGMA user_version=99")
    with blocked_stderr() as stderr:
        otel.start(stderr=stderr, RUST_LOG=log_filter)
        jsonl = data / "security-events.jsonl"
        if fault == "jsonl_write":
            jsonl.unlink()
            jsonl.mkdir()
        elif fault == "sqlite_write":
            with closing(sqlite3.connect(data / "security-events.db")) as connection:
                connection.execute(
                    "CREATE TRIGGER reject_event BEFORE INSERT ON security_events "
                    "BEGIN SELECT RAISE(FAIL, 'injected write failure'); END"
                )
                connection.commit()
        result = otel.cli(command=["scan-code", "--code", "echo safe"])
        assert result.returncode == 0, result.stderr
        assert json.loads(result.stdout)["verdict"] == "pass"
        # A diagnostic failure must not suppress the independent audit destination.
        if fault == "jsonl_write":
            with closing(sqlite3.connect(data / "security-events.db")) as connection:
                assert (
                    connection.execute(
                        "SELECT count(*) FROM security_events"
                    ).fetchone()[0]
                    == 1
                )
        else:
            assert len(jsonl.read_text().splitlines()) == 1
        otel.stop()
