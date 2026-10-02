"""Production composition acceptance: real binaries, UDS, SQLite and telemetry JSONL.

Build first with cargo build --locked -p asc-cli -p asc-daemon from v2/.
ASC_V2_BIN_DIR may select another directory of binaries explicitly.
"""

import fcntl
import json
import os
import shutil
import socket
import sqlite3
import subprocess
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

COMPONENT = Path(__file__).resolve().parents[3]
BIN_DIR = Path(os.environ.get("ASC_V2_BIN_DIR", COMPONENT / "v2/target/debug"))


def binary(name):
    candidate = BIN_DIR / name
    if candidate.is_file():
        return str(candidate)
    # Installed-RPM acceptance has no Cargo target directory.
    installed = shutil.which(name) if "ASC_V2_BIN_DIR" not in os.environ else None
    assert (
        installed is not None
    ), f"Build {name} in {BIN_DIR} or explicitly select installed binaries"
    return installed


def lines(path):
    return [json.loads(line) for line in path.read_text().splitlines()]


class ProcessDaemon:
    def __init__(self, root):
        self.root = root
        self.data = root / "data"
        self.socket = root / "daemon.sock"
        self.telemetry = root / "telemetry.jsonl"
        self.audit = self.data / "security-events.jsonl"
        self.db = self.data / "security-events.db"
        self.telemetry.touch()
        self.process = None
        self.log = None

    def start(self):
        # Never disable the host's privacy policy merely to run a test.
        assert not os.path.lexists(
            "/etc/anolisa/.telemetry_disabled"
        ), "Run enabled-telemetry acceptance on a test host whose policy permits it"
        env = os.environ.copy()
        env.update(
            AGENT_SEC_DATA_DIR=str(self.data),
            AGENT_SEC_TELEMETRY_LOG_PATH=str(self.telemetry),
        )
        self.log = (self.root / "daemon.stderr").open("a")
        self.process = subprocess.Popen(
            [binary("agent-sec-daemon"), "serve", "--socket", str(self.socket)],
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=self.log,
        )
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            assert self.process.poll() is None, (
                self.root / "daemon.stderr"
            ).read_text()
            try:
                response = self.rpc(
                    {"method": "lifecycle.readiness.probe", "params": {}}
                )
                assert response["error"]["code"] == "unknown_method"
                return
            except (FileNotFoundError, ConnectionRefusedError):
                time.sleep(0.01)
        raise AssertionError("daemon did not respond")

    def stop(self):
        if self.process is not None:
            self.process.terminate()
            try:
                assert self.process.wait(timeout=10) == 0
            finally:
                if self.process.poll() is None:
                    self.process.kill()
                    self.process.wait(timeout=5)
                self.process = None
                self.log.close()
            assert not self.socket.exists()

    def scan(
        self,
        code="echo SECRET_REQUEST",
        mode="regex",
        trace_context=None,
        otel_context=None,
    ):
        context_args = []
        for flag, context in (
            ("--trace-context", trace_context),
            ("--otel-context", otel_context),
        ):
            if context is not None:
                context_args.extend([flag, json.dumps(context)])
        result = subprocess.run(
            [
                binary("agent-sec-cli"),
                *context_args,
                "--socket",
                str(self.socket),
                "scan-code",
                "--code",
                code,
                "--mode",
                mode,
            ],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        assert result.stderr == "", result.stderr
        payload = json.loads(result.stdout)
        assert result.returncode == int(not payload["ok"])
        return payload

    def rpc(self, request):
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
            stream.settimeout(5)
            stream.connect(str(self.socket))
            stream.sendall(json.dumps(request).encode() + b"\n")
            with stream.makefile("rb") as response:
                return json.loads(response.readline())

    def rows(self):
        # Test-only independent connection: product CLI must never bypass daemon authorization.
        with sqlite3.connect(f"file:{self.db}?mode=ro", uri=True) as connection:
            connection.row_factory = sqlite3.Row
            records = [
                dict(row) for row in connection.execute("SELECT * FROM security_events")
            ]
        for record in records:
            record["details"] = json.loads(record["details"])
        return records


@pytest.fixture
def process_daemon(tmp_path):
    daemon = ProcessDaemon(tmp_path)
    try:
        daemon.start()
        yield daemon
    finally:
        daemon.stop()


def test_dproc_scan_emission_is_visible_before_response_and_survives_restart(
    process_daemon,
):
    daemon = process_daemon
    results = [
        daemon.scan(),
        daemon.scan("rm -rf /tmp/SECRET_REQUEST"),
        daemon.scan(mode="llm"),
    ]
    assert [r["verdict"] for r in results] == ["pass", "warn", "error"]
    events = lines(daemon.audit)
    telemetry = lines(daemon.telemetry)
    rows = {row["event_id"]: row for row in daemon.rows()}
    assert len(events) == len(telemetry) == len(rows) == 3
    for event, record, result in zip(events, telemetry, results):
        row = rows[event["event_id"]]
        for field, value in event.items():
            assert row[field] == value
        assert event["details"]["result"] == result
        assert event["uid"] == os.getuid()
        assert event["pid"] != daemon.process.pid
        assert record["seccore.verdict"] == result["verdict"]
        assert record["seccore.elapsed_ms"] == result["elapsed_ms"]
        assert record["seccore.timestamp"] == event["timestamp"]
        assert record["seccore.result"] == event["result"]
        assert set(record) <= {
            "component.name",
            "component.version",
            "component.agent_name",
            "seccore.event_type",
            "seccore.category",
            "seccore.result",
            "seccore.timestamp",
            "seccore.verdict",
            "seccore.elapsed_ms",
            "seccore.error_type",
            "seccore.exit_code",
        }
    assert "SECRET_REQUEST" not in daemon.telemetry.read_text()
    daemon.stop()
    daemon.start()
    assert set(rows) == {row["event_id"] for row in daemon.rows()}
    daemon.scan()
    assert (
        len(daemon.rows())
        == len(lines(daemon.audit))
        == len(lines(daemon.telemetry))
        == 4
    )


def test_each_destination_can_fail_without_suppressing_the_other_two(process_daemon):
    daemon = process_daemon
    context = {"agent_name": "codex", "session_id": "PRIVATE_FAILURE"}
    daemon.scan(trace_context=context)
    daemon.audit.rename(daemon.root / "previous-audit")
    daemon.audit.mkdir()
    assert daemon.scan(trace_context=context)["verdict"] == "pass"
    assert len(daemon.rows()) == len(lines(daemon.telemetry)) == 2
    daemon.audit.rmdir()
    with sqlite3.connect(daemon.db, timeout=0.1) as blocker:
        blocker.execute("BEGIN IMMEDIATE")
        assert daemon.scan(trace_context=context)["verdict"] == "pass"
        blocker.rollback()
    assert len(daemon.rows()) == 2
    assert len(lines(daemon.audit)) == 1
    assert len(lines(daemon.telemetry)) == 3
    with daemon.telemetry.open("a") as locked:
        fcntl.flock(locked, fcntl.LOCK_EX | fcntl.LOCK_NB)
        assert daemon.scan(trace_context=context)["verdict"] == "pass"
    assert len(daemon.rows()) == 3
    assert len(lines(daemon.telemetry)) == 3
    assert all(
        record["component.agent_name"] == "codex" for record in lines(daemon.telemetry)
    )
    assert "PRIVATE_FAILURE" not in daemon.telemetry.read_text()
    daemon.telemetry.unlink()
    assert daemon.scan(trace_context=context)["verdict"] == "pass"
    assert not daemon.telemetry.exists()
    assert len(daemon.rows()) == 4
    daemon.telemetry.mkdir()
    assert daemon.scan(trace_context=context)["verdict"] == "pass"
    assert len(daemon.rows()) == 5
    log = (daemon.root / "daemon.stderr").read_text()
    assert "skipped" in log
    assert "SECRET_REQUEST" not in log
    assert all(row["session_id"] == "PRIVATE_FAILURE" for row in daemon.rows())


def test_rejected_requests_never_enter_lifecycle(process_daemon):
    daemon = process_daemon
    for method, params, error in [
        ("action.code_scan", {}, "invalid_request"),
        ("action.unknown", {}, "unknown_method"),
    ]:
        assert (
            daemon.rpc({"method": method, "params": params})["error"]["code"] == error
        )
    assert daemon.rows() == lines(daemon.audit) == lines(daemon.telemetry) == []
    response = daemon.rpc(
        {"method": "action.code_scan", "params": {"code": "puts 1", "language": "ruby"}}
    )
    assert response["error"]["code"] == "invalid_argument"
    assert (
        len(daemon.rows())
        == len(lines(daemon.audit))
        == len(lines(daemon.telemetry))
        == 1
    )
    assert lines(daemon.telemetry)[0]["seccore.error_type"] == "ErrUnsupportedLang"


@pytest.mark.parametrize("mode", ["regex", "llm"])
def test_context_reaches_audit_with_only_allowlisted_agent_in_telemetry(
    process_daemon, mode
):
    daemon = process_daemon
    fields = ("trace_id", "session_id", "run_id", "call_id", "tool_call_id")
    legacy = {field: f"PRIVATE_LEGACY_{field}" for field in fields}
    native = {
        field: f"PRIVATE_NATIVE_{field}" for field in fields if field != "trace_id"
    }
    carrier = {
        "version": 1,
        "traceparent": "00-11111111111111111111111111111111-2222222222222222-00",
        "baggage": "agentsec.agent.name=hermes,"
        + ",".join(
            f"agentsec.{field.removesuffix('_id')}.id={value}"
            for field, value in native.items()
        )
        + ",unknown=PRIVATE_UNKNOWN",
    }
    # Compatibility input overrides native baggage while SDK identity remains separate.
    daemon.scan(
        mode=mode, trace_context=legacy | {"agentName": " codex "}, otel_context=carrier
    )
    daemon.scan(mode=mode, otel_context=carrier)
    daemon.scan(mode=mode)
    daemon.scan(mode=mode, trace_context={"agent_name": "PRIVATE_UNAPPROVED_AGENT"})
    events, records = lines(daemon.audit), lines(daemon.telemetry)
    rows = {row["event_id"]: row for row in daemon.rows()}
    assert len(events) == len(records) == len(rows) == 4
    empty = {field: "" if field == "trace_id" else None for field in fields}
    for event, expected in zip(
        events, [legacy, native | {"trace_id": ""}, empty, empty]
    ):
        row = rows[event["event_id"]]
        assert {field: event.get(field) for field in fields} == expected
        assert {field: row[field] for field in fields} == expected
        assert event["uid"] == os.getuid()
        assert "agent_name" not in event
    assert [record["component.agent_name"] for record in records] == [
        "codex",
        "hermes",
        "",
        "",
    ]
    assert "PRIVATE_" not in daemon.telemetry.read_text()
    assert all(
        not any(field in key for field in fields)
        for record in records
        for key in record
    )


def test_concurrent_requests_finalize_once_without_audit_loss(process_daemon):
    daemon = process_daemon
    with ThreadPoolExecutor(max_workers=8) as pool:
        results = list(
            pool.map(
                lambda i: daemon.scan(
                    trace_context={
                        "trace_id": f"PRIVATE_TRACE_{i}",
                        "session_id": f"PRIVATE_SESSION_{i}",
                        "run_id": f"PRIVATE_RUN_{i}",
                        "call_id": f"PRIVATE_CALL_{i}",
                        "tool_call_id": f"PRIVATE_TOOL_{i}",
                        "agent_name": "codex",
                    }
                ),
                range(16),
            )
        )
    assert all(r["verdict"] == "pass" for r in results)
    rows, events, telemetry = (
        daemon.rows(),
        lines(daemon.audit),
        lines(daemon.telemetry),
    )
    assert len(rows) == len(events) == 16
    assert len({event["event_id"] for event in events}) == 16
    assert {row["trace_id"] for row in rows} == {
        f"PRIVATE_TRACE_{i}" for i in range(16)
    }
    by_id = {row["event_id"]: row for row in rows}
    for event in events:
        i = event["trace_id"].removeprefix("PRIVATE_TRACE_")
        for field, label in (
            ("session_id", "SESSION"),
            ("run_id", "RUN"),
            ("call_id", "CALL"),
            ("tool_call_id", "TOOL"),
        ):
            assert (
                event[field]
                == by_id[event["event_id"]][field]
                == f"PRIVATE_{label}_{i}"
            )
    # V1 telemetry intentionally skips a contended nonblocking lock.
    assert 0 < len(telemetry) <= 16
    assert all(record["seccore.verdict"] == "pass" for record in telemetry)
    assert all(record["component.agent_name"] == "codex" for record in telemetry)
    assert "PRIVATE_" not in daemon.telemetry.read_text()
