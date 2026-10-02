"""E2E tests for the agent-sec daemon process."""

import json
import os
import shutil
import signal
import socket
import sqlite3
import stat
import subprocess
import sys
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pytest


@dataclass
class DaemonOutput:
    stdout: str
    stderr: str
    returncode: int


@pytest.fixture
def daemon_command() -> list[str]:
    """Return the installed daemon binary or a source-tree module fallback."""
    daemon_bin = shutil.which("agent-sec-daemon")
    if daemon_bin:
        return [daemon_bin]

    result = subprocess.run(
        [sys.executable, "-c", "import agent_sec_cli.daemon.server"],
        capture_output=True,
        check=False,
        text=True,
        timeout=10,
    )
    if result.returncode == 0:
        return [sys.executable, "-m", "agent_sec_cli.daemon.server"]

    pytest.skip("agent-sec-daemon is not installed and daemon module is not importable")


def test_daemon_health_over_unix_socket(
    daemon_command: list[str], tmp_path: Path
) -> None:
    socket_path = tmp_path / "runtime" / "daemon.sock"
    process = _start_daemon(daemon_command, socket_path, tmp_path)
    output: DaemonOutput | None = None

    try:
        response = _call_daemon(
            socket_path,
            {"id": "e2e-health", "method": "daemon.health"},
        )

        assert response["request_id"] != "e2e-health"
        _assert_uuid(response["request_id"])
        assert response["ok"] is True
        assert response["exit_code"] == 0
        assert response.get("error") is None
        assert response["data"]["status"] == "ok"
        assert response["data"]["socket"] == str(socket_path)
        jobs = {job["name"]: job for job in response["data"]["jobs"]}
        assert jobs["skill-ledger-activation"]["state"] == "running"
        assert "inflight" in response["data"]["queues"]
        assert "queued" in response["data"]["queues"]
        assert stat.S_IMODE(socket_path.parent.stat().st_mode) == 0o700
        assert stat.S_IMODE(socket_path.stat().st_mode) == 0o600
    finally:
        output = _stop_daemon(process)

    assert not socket_path.exists()
    assert output.returncode == 0
    assert not _has_request_log(tmp_path, response["request_id"], "daemon.health")


def test_daemon_security_events_list_reads_sqlite_over_unix_socket(
    daemon_command: list[str], tmp_path: Path
) -> None:
    socket_path = tmp_path / "runtime" / "daemon.sock"
    _write_security_event(tmp_path / "data" / "security-events.db")
    process = _start_daemon(daemon_command, socket_path, tmp_path)

    try:
        response = _call_daemon(
            socket_path,
            {
                "method": "sec.events.list",
                "params": {
                    "session_id": "session-e2e",
                    "include_details": True,
                },
            },
        )
    finally:
        output = _stop_daemon(process)

    assert response["ok"] is True
    assert response["data"]["total"] == 1
    assert response["data"]["items"][0]["event_id"] == "event-e2e"
    assert response["data"]["items"][0]["details"]["request"]["code"] == "echo e2e"
    assert output.returncode == 0
    assert _has_request_log(tmp_path, response["request_id"], "sec.events.list")


def test_daemon_security_query_methods_read_sqlite_over_unix_socket(
    daemon_command: list[str], tmp_path: Path
) -> None:
    socket_path = tmp_path / "runtime" / "daemon.sock"
    _write_security_event(tmp_path / "data" / "security-events.db")
    process = _start_daemon(daemon_command, socket_path, tmp_path)

    try:
        summary_response = _call_daemon(
            socket_path,
            {
                "method": "sec.summary",
                "params": {"session_id": "session-e2e"},
            },
        )
        get_response = _call_daemon(
            socket_path,
            {
                "method": "sec.events.get",
                "params": {"event_id": "event-e2e"},
            },
        )
        count_response = _call_daemon(
            socket_path,
            {
                "method": "sec.events.count_by",
                "params": {
                    "group_by": "run_id",
                    "session_id": "session-e2e",
                },
            },
        )
    finally:
        output = _stop_daemon(process)

    assert summary_response["ok"] is True
    assert summary_response["data"]["total"] == 1
    assert summary_response["data"]["by_category"] == {"code_scan": 1}
    assert summary_response["data"]["affected_runs"] == 1
    assert summary_response["data"]["latest_events"][0]["event_id"] == "event-e2e"

    assert get_response["ok"] is True
    assert get_response["data"]["found"] is True
    assert get_response["data"]["event"]["event_id"] == "event-e2e"
    assert get_response["data"]["event"]["details"]["request"]["code"] == "echo e2e"

    assert count_response["ok"] is True
    assert count_response["data"]["group_by"] == "run_id"
    assert count_response["data"]["items"] == [{"value": "run-e2e", "count": 1}]

    assert output.returncode == 0
    assert _has_request_log(tmp_path, summary_response["request_id"], "sec.summary")
    assert _has_request_log(tmp_path, get_response["request_id"], "sec.events.get")
    assert _has_request_log(
        tmp_path,
        count_response["request_id"],
        "sec.events.count_by",
    )


def test_daemon_uses_xdg_runtime_dir_by_default(
    daemon_command: list[str], tmp_path: Path
) -> None:
    xdg_runtime_dir = tmp_path / "xdg"
    socket_path = xdg_runtime_dir / "agent-sec-core" / "daemon.sock"
    process = _start_daemon(
        daemon_command,
        socket_path,
        tmp_path,
        use_default_socket=True,
        xdg_runtime_dir=xdg_runtime_dir,
    )

    try:
        response = _call_daemon(
            socket_path,
            {"id": "e2e-default-socket", "method": "daemon.health"},
        )
    finally:
        output = _stop_daemon(process)

    assert response["ok"] is True
    assert response["data"]["socket"] == str(socket_path)
    assert not socket_path.exists()
    assert output.returncode == 0


def test_daemon_unknown_method_returns_structured_error(
    daemon_command: list[str], tmp_path: Path
) -> None:
    socket_path = tmp_path / "runtime" / "daemon.sock"
    process = _start_daemon(daemon_command, socket_path, tmp_path)

    try:
        response = _call_daemon(
            socket_path,
            {"id": "e2e-unknown", "method": "unknown.method"},
        )
    finally:
        output = _stop_daemon(process)

    assert response["request_id"] != "e2e-unknown"
    _assert_uuid(response["request_id"])
    assert response["ok"] is False
    assert response["exit_code"] == 1
    assert response["stderr"] == "unknown daemon method: unknown.method"
    assert response["error"] == {
        "code": "unknown_method",
        "message": "unknown daemon method: unknown.method",
    }
    assert output.returncode == 0
    assert _has_request_event(
        tmp_path,
        "daemon_request_started",
        response["request_id"],
        "unknown.method",
    )
    assert _has_request_log(tmp_path, response["request_id"], "unknown.method")


def test_daemon_returns_busy_when_connection_limit_is_exhausted(
    daemon_command: list[str], tmp_path: Path
) -> None:
    socket_path = tmp_path / "runtime" / "daemon.sock"
    process = _start_daemon(
        daemon_command,
        socket_path,
        tmp_path,
        max_connections=0,
        wait_for_health=False,
    )

    try:
        _wait_for_socket(socket_path, process)
        response = _call_daemon(
            socket_path,
            {"id": "e2e-busy", "method": "daemon.health"},
        )
    finally:
        output = _stop_daemon(process)

    assert response["ok"] is False
    _assert_uuid(response["request_id"])
    assert response["exit_code"] == 1
    assert response["error"] == {"code": "busy", "message": "daemon is busy"}
    assert output.returncode == 0


def test_daemon_idle_client_times_out_and_releases_connection(
    daemon_command: list[str], tmp_path: Path
) -> None:
    socket_path = tmp_path / "runtime" / "daemon.sock"
    process = _start_daemon(
        daemon_command,
        socket_path,
        tmp_path,
        max_connections=1,
        request_read_timeout_ms=100,
    )

    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as idle_socket:
            idle_socket.settimeout(5)
            idle_socket.connect(str(socket_path))
            timeout_response = json.loads(_read_response(idle_socket).decode("utf-8"))

        health_response = _call_daemon(
            socket_path,
            {"id": "e2e-after-idle-timeout", "method": "daemon.health"},
        )
    finally:
        output = _stop_daemon(process)

    assert timeout_response["ok"] is False
    _assert_uuid(timeout_response["request_id"])
    assert timeout_response["error"]["code"] == "timeout"
    assert health_response["ok"] is True
    _assert_uuid(health_response["request_id"])
    assert output.returncode == 0


def test_daemon_sigterm_graceful_shutdown(
    daemon_command: list[str], tmp_path: Path
) -> None:
    socket_path = tmp_path / "runtime" / "daemon.sock"
    process = _start_daemon(daemon_command, socket_path, tmp_path)

    output = _stop_daemon(process, stop_signal=signal.SIGTERM)

    assert output.returncode == 0
    assert not socket_path.exists()
    assert _has_daemon_event(tmp_path, "daemon_started")
    assert _has_daemon_event(tmp_path, "daemon_stopped")


def test_daemon_skillfs_notify_refreshes_skill_ledger_activation(
    daemon_command: list[str], tmp_path: Path
) -> None:
    socket_path = tmp_path / "runtime" / "daemon.sock"
    skill_dir = _make_skill(tmp_path / "skills", "weather")
    process = _start_daemon(daemon_command, socket_path, tmp_path)

    try:
        response = _call_daemon(
            socket_path,
            {
                "method": "skill_ledger.skillfs_notify_change",
                "params": {
                    "schemaVersion": 2,
                    "canonicalSkillDir": str(skill_dir),
                    "skillId": "weather",
                    "eventKind": "write",
                    "paths": ["SKILL.md"],
                },
            },
        )
        activation = _wait_for_activation(skill_dir, process)
    finally:
        output = _stop_daemon(process)

    assert response["ok"] is True
    assert response["data"]["accepted"] is True
    assert activation == {
        "schemaVersion": 1,
        "target": ".skill-meta/versions/v000001.snapshot",
    }
    assert output.returncode == 0
    assert _has_request_log(
        tmp_path,
        response["request_id"],
        "skill_ledger.skillfs_notify_change",
    )


def test_daemon_skillfs_notify_rejects_unknown_fields_without_state_change(
    daemon_command: list[str], tmp_path: Path
) -> None:
    socket_path = tmp_path / "runtime" / "daemon.sock"
    skill_dir = _make_skill(tmp_path / "skills", "weather")
    original_files = sorted(
        path.relative_to(skill_dir) for path in skill_dir.rglob("*")
    )
    listxattr = getattr(os, "listxattr", None)
    original_xattrs = listxattr(skill_dir) if listxattr is not None else None
    process = _start_daemon(daemon_command, socket_path, tmp_path)

    requests = [
        {
            "schemaVersion": 2,
            "canonicalSkillDir": str(skill_dir),
            "skillId": "weather",
            "eventKind": "unlink",
            "paths": ["SKILL.md"],
            "exists": False,
        },
        {
            "schemaVersion": 2,
            "canonicalSkillDir": str(skill_dir),
            "skillId": "weather",
            "eventKind": "unlink",
            "paths": ["SKILL.md"],
            "exists": True,
        },
        {
            "schemaVersion": 2,
            "canonicalSkillDir": str(skill_dir),
            "skillId": "weather",
            "eventKind": "write",
            "paths": [".skill-meta/latest.json"],
            "futureField": "unsupported",
        },
    ]

    try:
        responses = [
            _call_daemon(
                socket_path,
                {
                    "method": "skill_ledger.skillfs_notify_change",
                    "params": params,
                },
            )
            for params in requests
        ]
        time.sleep(1)
    finally:
        output = _stop_daemon(process)

    assert all(response["ok"] is False for response in responses)
    assert all(response["error"]["code"] == "bad_request" for response in responses)
    assert all(
        "unknown fields" in response["error"]["message"] for response in responses
    )
    assert sorted(path.relative_to(skill_dir) for path in skill_dir.rglob("*")) == (
        original_files
    )
    if original_xattrs is not None:
        assert listxattr(skill_dir) == original_xattrs
    assert not (skill_dir / ".skill-meta").exists()
    assert output.returncode == 0


def _write_security_event(db_path: Path) -> None:
    db_path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(db_path)
    try:
        conn.executescript("""
            PRAGMA user_version = 3;
            CREATE TABLE IF NOT EXISTS security_events (
                event_id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                category TEXT NOT NULL,
                result TEXT NOT NULL DEFAULT 'succeeded',
                timestamp TEXT NOT NULL,
                timestamp_epoch FLOAT NOT NULL,
                trace_id TEXT NOT NULL DEFAULT '',
                pid INTEGER NOT NULL,
                uid INTEGER NOT NULL,
                session_id TEXT,
                run_id TEXT,
                call_id TEXT,
                tool_call_id TEXT,
                verdict TEXT,
                details TEXT NOT NULL
            );
            """)
        conn.execute(
            """
            INSERT INTO security_events (
                event_id,
                event_type,
                category,
                result,
                timestamp,
                timestamp_epoch,
                trace_id,
                pid,
                uid,
                session_id,
                run_id,
                call_id,
                tool_call_id,
                verdict,
                details
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                "event-e2e",
                "code_scan",
                "code_scan",
                "succeeded",
                "2026-06-09T00:00:00+00:00",
                1780963200.0,
                "trace-e2e",
                os.getpid(),
                os.getuid(),
                "session-e2e",
                "run-e2e",
                None,
                "tool-e2e",
                None,
                json.dumps({"request": {"code": "echo e2e"}}),
            ),
        )
        conn.commit()
    finally:
        conn.close()


def _start_daemon(
    daemon_command: list[str],
    socket_path: Path,
    tmp_path: Path,
    max_connections: int = 64,
    request_read_timeout_ms: int = 5000,
    wait_for_health: bool = True,
    use_default_socket: bool = False,
    xdg_runtime_dir: Path | None = None,
) -> subprocess.Popen[str]:
    env = os.environ.copy()
    env.pop("AGENT_SEC_DAEMON_SOCKET", None)
    env["AGENT_SEC_DATA_DIR"] = str(tmp_path / "data")
    env["XDG_CONFIG_HOME"] = str(tmp_path / "xdg_config")
    env["XDG_DATA_HOME"] = str(tmp_path / "xdg_data")
    env["PYTHONUNBUFFERED"] = "1"
    if xdg_runtime_dir is not None:
        env["XDG_RUNTIME_DIR"] = str(xdg_runtime_dir)
    _write_skill_ledger_config(tmp_path)

    command = [*daemon_command, "serve"]
    if not use_default_socket:
        command.extend(["--socket", str(socket_path)])
    command.extend(["--max-connections", str(max_connections)])
    command.extend(["--request-read-timeout-ms", str(request_read_timeout_ms)])

    process = subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )
    if wait_for_health:
        _wait_for_health(socket_path, process)
    return process


def _write_skill_ledger_config(tmp_path: Path) -> None:
    config_dir = tmp_path / "xdg_config" / "agent-sec" / "skill-ledger"
    config_dir.mkdir(parents=True, exist_ok=True)
    (config_dir / "config.json").write_text(
        json.dumps(
            {
                "enableDefaultSkillDirs": False,
                "managedSkillDirs": [],
            }
        ),
        encoding="utf-8",
    )


def _make_skill(parent: Path, name: str) -> Path:
    skill_dir = parent / name
    skill_dir.mkdir(parents=True)
    (skill_dir / "SKILL.md").write_text(
        f"---\nname: {name}\ndescription: Test skill\n---\n# {name}\n",
        encoding="utf-8",
    )
    (skill_dir / "run.sh").write_text("echo ok\n", encoding="utf-8")
    return skill_dir


def _wait_for_activation(skill_dir: Path, process: subprocess.Popen[str]) -> dict:
    activation_path = skill_dir / ".skill-meta" / "activation.json"
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        _assert_process_running(process)
        if activation_path.is_file():
            return json.loads(activation_path.read_text(encoding="utf-8"))
        time.sleep(0.05)
    raise AssertionError(f"activation was not written: {activation_path}")


def _stop_daemon(
    process: subprocess.Popen[str],
    stop_signal: signal.Signals = signal.SIGINT,
) -> DaemonOutput:
    if process.poll() is None:
        process.send_signal(stop_signal)

    try:
        stdout, stderr = process.communicate(timeout=10)
    except subprocess.TimeoutExpired:
        process.terminate()
        try:
            stdout, stderr = process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            stdout, stderr = process.communicate(timeout=5)

    return DaemonOutput(
        stdout=stdout,
        stderr=stderr,
        returncode=0 if process.returncode is None else process.returncode,
    )


def _wait_for_health(socket_path: Path, process: subprocess.Popen[str]) -> None:
    deadline = time.monotonic() + 10
    last_error: Exception | None = None

    while time.monotonic() < deadline:
        _assert_process_running(process)
        if socket_path.exists():
            try:
                response = _call_daemon(
                    socket_path,
                    {"id": "e2e-wait-health", "method": "daemon.health"},
                )
            except OSError as exc:
                last_error = exc
            else:
                if response.get("ok") is True:
                    return
        time.sleep(0.5)

    raise AssertionError(f"daemon did not become healthy; last_error={last_error!r}")


def _wait_for_socket(socket_path: Path, process: subprocess.Popen[str]) -> None:
    deadline = time.monotonic() + 10

    while time.monotonic() < deadline:
        _assert_process_running(process)
        if socket_path.exists():
            return
        time.sleep(0.05)

    raise AssertionError(f"daemon socket was not created: {socket_path}")


def _assert_process_running(process: subprocess.Popen[str]) -> None:
    if process.poll() is None:
        return

    stdout, stderr = process.communicate(timeout=1)
    raise AssertionError(
        f"daemon exited before test request; returncode={process.returncode}; "
        f"stdout={stdout!r}; stderr={stderr!r}"
    )


def _call_daemon(socket_path: Path, request: dict[str, Any]) -> dict[str, Any]:
    raw_request = json.dumps(request, separators=(",", ":")).encode("utf-8") + b"\n"
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client_socket:
        client_socket.settimeout(5)
        client_socket.connect(str(socket_path))
        client_socket.sendall(raw_request)
        raw_response = _read_response(client_socket)

    response = json.loads(raw_response.decode("utf-8"))
    assert isinstance(response, dict)
    return response


def _assert_uuid(value: str) -> None:
    uuid.UUID(value)


def _read_response(client_socket: socket.socket) -> bytes:
    chunks: list[bytes] = []
    total_bytes = 0

    while True:
        chunk = client_socket.recv(4096)
        if not chunk:
            break
        chunks.append(chunk)
        total_bytes += len(chunk)
        if total_bytes > 4 * 1024 * 1024:
            raise AssertionError("daemon response exceeded e2e read limit")
        if b"\n" in chunk:
            break

    if not chunks:
        raise AssertionError("daemon returned an empty response")

    raw_response, _separator, _remaining = b"".join(chunks).partition(b"\n")
    return raw_response


def _has_request_log(tmp_path: Path, request_id: str, method: str | None) -> bool:
    for payload in _read_daemon_log_payloads(tmp_path):
        data = payload.get("data", {})
        if (
            payload.get("event") == "daemon_request_completed"
            and payload.get("request_id") == request_id
            and isinstance(data, dict)
            and data.get("method") == method
            and "latency_ms" in data
            and "bytes_in" in data
            and "bytes_out" in data
        ):
            return True
    return False


def _has_request_event(
    tmp_path: Path,
    event: str,
    request_id: str,
    method: str | None,
) -> bool:
    return any(
        payload.get("event") == event
        and payload.get("request_id") == request_id
        and isinstance(payload.get("data"), dict)
        and payload["data"].get("method") == method
        for payload in _read_daemon_log_payloads(tmp_path)
    )


def _has_daemon_event(tmp_path: Path, event: str) -> bool:
    return any(
        payload.get("event") == event for payload in _read_daemon_log_payloads(tmp_path)
    )


def _read_daemon_log_payloads(tmp_path: Path) -> list[dict[str, Any]]:
    log_path = tmp_path / "data" / "daemon.jsonl"
    if not log_path.exists():
        return []

    payloads: list[dict[str, Any]] = []
    for line in log_path.read_text(encoding="utf-8").splitlines():
        try:
            payload = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(payload, dict):
            payloads.append(payload)
    return payloads
