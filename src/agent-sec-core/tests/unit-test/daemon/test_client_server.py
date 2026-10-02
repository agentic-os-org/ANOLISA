"""Tests for daemon client/server integration."""

import asyncio
import contextlib
import json
import logging
import os
import socket
import stat
import sys
import time
import uuid
from pathlib import Path

import pytest
from agent_sec_cli.correlation_context import (
    TraceContext,
    clear_invocation_context_for_tests,
    clear_process_trace_context,
    get_current_trace_context,
    init_invocation_context,
    init_process_trace_context,
)
from agent_sec_cli.daemon import server as daemon_server_module
from agent_sec_cli.daemon.client import DaemonClient, daemon_health_reachable
from agent_sec_cli.daemon.errors import (
    DaemonProtocolError,
    DaemonRuntimePathError,
)
from agent_sec_cli.daemon.handlers.skill_ledger import (
    METHOD_SKILLFS_NOTIFY_CHANGE,
)
from agent_sec_cli.daemon.health import build_health_snapshot
from agent_sec_cli.daemon.logging import (
    reset_daemon_diagnostic_logging_for_tests,
    setup_daemon_logging,
)
from agent_sec_cli.daemon.protocol import (
    DaemonRequest,
    DaemonResponse,
    parse_response_line,
    serialize_request,
    success_response,
)
from agent_sec_cli.daemon.registry import (
    HandlerResult,
    MethodRegistry,
    MethodSpec,
)
from agent_sec_cli.daemon.request_context import daemon_request_context
from agent_sec_cli.daemon.runtime import DaemonRuntime, resolve_socket_path
from agent_sec_cli.daemon.server import (
    DaemonServer,
    _log_request_completion,
    configure_logging,
    create_default_registry,
    prepare_socket_path,
)
from agent_sec_cli.skill_ledger.skillfs_peer_auth import (
    AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE,
    NOTIFY_CLIENT_DOMAIN,
    NOTIFY_SERVER_DOMAIN,
    AuthenticatedSession,
    SharedSecret,
    SkillFsPeerAuthError,
    build_auth_init_frame,
    build_auth_proof_frame,
    parse_auth_challenge_frame,
    verify_auth_proof_frame,
)


def _matching_modules(prefixes: tuple[str, ...]) -> tuple[str, ...]:
    return tuple(sorted(name for name in sys.modules if name.startswith(prefixes)))


def _assert_uuid(value: str | None) -> None:
    assert value is not None
    uuid.UUID(value)


def test_client_uses_env_socket_override(monkeypatch, tmp_path: Path):
    socket_path = tmp_path / "runtime" / "daemon.sock"
    monkeypatch.setenv("AGENT_SEC_DAEMON_SOCKET", str(socket_path))

    client = DaemonClient()

    assert client.socket_path == socket_path


def test_resolve_socket_path_falls_back_to_run_user_when_xdg_missing(monkeypatch):
    monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
    monkeypatch.setattr("agent_sec_cli.daemon.runtime.os.getuid", lambda: 1234)
    monkeypatch.setattr(
        "agent_sec_cli.daemon.runtime.os.path.isdir",
        lambda path: path == "/run/user/1234",
    )

    socket_path = resolve_socket_path(use_env=False)

    assert socket_path == Path("/run/user/1234/agent-sec-core/daemon.sock")


def test_resolve_socket_path_rejects_missing_xdg_and_run_user(monkeypatch):
    monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
    monkeypatch.setattr("agent_sec_cli.daemon.runtime.os.getuid", lambda: 5678)
    monkeypatch.setattr(
        "agent_sec_cli.daemon.runtime.os.path.isdir",
        lambda _path: False,
    )

    try:
        resolve_socket_path(use_env=False)
    except DaemonRuntimePathError as exc:
        assert exc.message == "XDG_RUNTIME_DIR is required for agent-sec daemon"
    else:
        raise AssertionError("expected missing runtime directory to fail")


def test_configure_logging_does_not_install_console_handler(monkeypatch):
    setup_calls = []
    root_logger = logging.getLogger()
    logger = logging.getLogger("agent-sec-core.daemon")
    original_root_level = root_logger.level
    original_level = logger.level
    original_propagate = logger.propagate

    def fake_setup_daemon_logging():
        setup_calls.append(True)

    def fail_basic_config(*_args, **_kwargs):
        raise AssertionError("daemon logging must not configure stdout/stderr")

    monkeypatch.setattr(
        "agent_sec_cli.daemon.server.setup_daemon_logging",
        fake_setup_daemon_logging,
    )
    monkeypatch.setattr(logging, "basicConfig", fail_basic_config)

    try:
        configure_logging()

        assert setup_calls == [True]
        assert root_logger.level == logging.DEBUG
        assert logger.level == logging.NOTSET
        assert logger.propagate is True
    finally:
        root_logger.setLevel(original_root_level)
        logger.setLevel(original_level)
        logger.propagate = original_propagate


def test_daemon_client_rejects_oversized_response(tmp_path: Path):
    server_socket, client_socket = socket.socketpair()
    try:
        client = DaemonClient(
            socket_path=tmp_path / "unused.sock", max_response_bytes=8
        )
        server_socket.sendall(
            b'{"request_id":"req-1","ok":true,"data":{},"stdout":"","stderr":"","exit_code":0}\n'
        )

        try:
            client._read_response_line(client_socket)
        except DaemonProtocolError as exc:
            assert "exceeds byte limit" in str(exc)
        else:
            raise AssertionError("expected oversized daemon response to fail")
    finally:
        server_socket.close()
        client_socket.close()


def test_daemon_client_fills_missing_trace_id_from_invocation_id(
    monkeypatch,
    tmp_path: Path,
):
    clear_invocation_context_for_tests()
    monkeypatch.setenv("AGENT_SEC_INVOCATION_ID", "invocation-1")
    init_invocation_context()
    client = DaemonClient(socket_path=tmp_path / "unused.sock")
    captured = {}

    def fake_send_request(request: DaemonRequest, _timeout_ms: int) -> DaemonResponse:
        captured["request"] = request
        return DaemonResponse(request_id=request.request_id, ok=True)

    monkeypatch.setattr(client, "_send_request", fake_send_request)

    try:
        client.call(
            "scan-prompt",
            trace_context={"session_id": "session-1"},
        )
    finally:
        clear_invocation_context_for_tests()

    request = captured["request"]
    assert request.caller is None
    assert request.trace_context == {
        "trace_id": "invocation-1",
        "session_id": "session-1",
    }


def test_daemon_client_preserves_explicit_trace_id(
    monkeypatch,
    tmp_path: Path,
):
    clear_invocation_context_for_tests()
    monkeypatch.setenv("AGENT_SEC_INVOCATION_ID", "invocation-1")
    init_invocation_context()
    client = DaemonClient(socket_path=tmp_path / "unused.sock")
    trace_context = {"trace_id": "trace-1", "session_id": "session-1"}
    captured = {}

    def fake_send_request(request: DaemonRequest, _timeout_ms: int) -> DaemonResponse:
        captured["request"] = request
        return DaemonResponse(request_id=request.request_id, ok=True)

    monkeypatch.setattr(client, "_send_request", fake_send_request)

    try:
        client.call(
            "scan-prompt",
            trace_context=trace_context,
        )
    finally:
        clear_invocation_context_for_tests()

    request = captured["request"]
    assert request.caller is None
    assert request.trace_context == {
        "trace_id": "trace-1",
        "session_id": "session-1",
    }
    assert trace_context == {"trace_id": "trace-1", "session_id": "session-1"}


def test_daemon_client_requires_explicit_trace_context(tmp_path: Path):
    client = DaemonClient(socket_path=tmp_path / "unused.sock")

    try:
        # Runtime guard for callers that bypass static type checking.
        client.call("scan-prompt")
    except TypeError as exc:
        assert "trace_context" in str(exc)
    else:
        raise AssertionError("expected missing trace_context to fail")


def test_daemon_client_explicit_trace_context_overrides_ambient_context(
    monkeypatch,
    tmp_path: Path,
):
    clear_process_trace_context()
    init_process_trace_context(
        TraceContext(
            trace_id="trace-ambient",
            session_id="session-ambient",
            agent_name="hermes",
        )
    )
    client = DaemonClient(socket_path=tmp_path / "unused.sock")
    captured = {}

    def fake_send_request(request: DaemonRequest, _timeout_ms: int) -> DaemonResponse:
        captured["request"] = request
        return DaemonResponse(request_id=request.request_id, ok=True)

    monkeypatch.setattr(client, "_send_request", fake_send_request)

    try:
        client.call(
            "scan-prompt",
            trace_context={"trace_id": "trace-explicit", "agent_name": "cosh"},
        )
    finally:
        clear_process_trace_context()

    request = captured["request"]
    assert request.trace_context == {
        "trace_id": "trace-explicit",
        "agent_name": "cosh",
    }


def test_daemon_client_can_disable_ambient_trace_context_inheritance(
    monkeypatch,
    tmp_path: Path,
):
    clear_process_trace_context()
    clear_invocation_context_for_tests()
    monkeypatch.setenv("AGENT_SEC_INVOCATION_ID", "invocation-1")
    init_invocation_context()
    init_process_trace_context(
        TraceContext(
            trace_id="trace-ambient",
            session_id="session-ambient",
            agent_name="hermes",
        )
    )
    client = DaemonClient(socket_path=tmp_path / "unused.sock")
    captured = {}

    def fake_send_request(request: DaemonRequest, _timeout_ms: int) -> DaemonResponse:
        captured["request"] = request
        return DaemonResponse(request_id=request.request_id, ok=True)

    monkeypatch.setattr(client, "_send_request", fake_send_request)

    try:
        client.call("daemon.health", trace_context={})
    finally:
        clear_process_trace_context()
        clear_invocation_context_for_tests()

    request = captured["request"]
    assert request.trace_context == {"trace_id": "invocation-1"}


def test_daemon_health_reachable_disables_ambient_trace_context(monkeypatch) -> None:
    calls = []

    def fake_call(self, method: str, **kwargs) -> DaemonResponse:
        calls.append((self, method, kwargs))
        return DaemonResponse(request_id="req-health", ok=True)

    monkeypatch.setattr(DaemonClient, "call", fake_call)

    assert daemon_health_reachable(Path("/unused/daemon.sock")) is True
    assert calls[0][1] == "daemon.health"
    assert calls[0][2]["trace_context"] == {}


def test_daemon_client_allows_caller_override(
    monkeypatch,
    tmp_path: Path,
):
    client = DaemonClient(socket_path=tmp_path / "unused.sock")
    captured = {}

    def fake_send_request(request: DaemonRequest, _timeout_ms: int) -> DaemonResponse:
        captured["request"] = request
        return DaemonResponse(request_id=request.request_id, ok=True)

    monkeypatch.setattr(client, "_send_request", fake_send_request)

    client.call("daemon.health", trace_context={}, caller="health-check")

    request = captured["request"]
    assert request.caller == "health-check"


def test_daemon_write_response_closes_writer_when_drain_is_cancelled(
    tmp_path: Path,
):
    class BlockingDrainWriter:
        def __init__(self) -> None:
            self.drain_started = asyncio.Event()
            self.closed = False
            self.wait_closed_called = False
            self.wrote = False

        def write(self, _data: bytes) -> None:
            self.wrote = True

        async def drain(self) -> None:
            self.drain_started.set()
            await asyncio.Event().wait()

        def close(self) -> None:
            self.closed = True

        async def wait_closed(self) -> None:
            self.wait_closed_called = True

    async def scenario():
        writer = BlockingDrainWriter()
        server = DaemonServer(socket_path=tmp_path / "runtime" / "daemon.sock")
        write_task = asyncio.create_task(
            server._write_response(writer, success_response("req-cancel-write"))
        )
        await asyncio.wait_for(writer.drain_started.wait(), timeout=0.5)

        write_task.cancel()
        with contextlib.suppress(asyncio.CancelledError):
            await write_task

        return writer

    writer = asyncio.run(scenario())

    assert writer.wrote is True
    assert writer.closed is True
    assert writer.wait_closed_called is True


def test_daemon_server_uses_default_job_registration(monkeypatch, tmp_path: Path):
    registered_managers = []

    def fake_register_default_jobs(job_manager):
        registered_managers.append(job_manager)

    monkeypatch.setattr(
        "agent_sec_cli.daemon.server.register_default_jobs",
        fake_register_default_jobs,
    )

    server = DaemonServer(socket_path=tmp_path / "runtime" / "daemon.sock")

    assert registered_managers == [server.runtime.jobs]


def test_daemon_client_calls_health_over_temp_socket(tmp_path: Path):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        server = DaemonServer(socket_path=socket_path)
        await server.start()
        try:
            client = DaemonClient(socket_path=socket_path)
            response = await asyncio.to_thread(
                client.call,
                "daemon.health",
                trace_context={},
            )
            runtime_dir_mode = stat.S_IMODE(socket_path.parent.stat().st_mode)
            socket_mode = stat.S_IMODE(socket_path.stat().st_mode)
        finally:
            await server.stop()

        return response, runtime_dir_mode, socket_mode

    response, runtime_dir_mode, socket_mode = asyncio.run(scenario())

    _assert_uuid(response.request_id)
    assert response.ok is True
    assert response.exit_code == 0
    assert response.data["status"] == "ok"
    assert response.data["socket"].endswith("daemon.sock")
    assert runtime_dir_mode == 0o700
    assert socket_mode == 0o600


def test_daemon_start_closes_bound_server_when_chmod_fails(monkeypatch, tmp_path: Path):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        server = DaemonServer(socket_path=socket_path)
        original_chmod = os.chmod

        def fail_socket_chmod(path: str | Path, mode: int) -> None:
            if Path(path) == socket_path:
                raise PermissionError("forced chmod failure")
            original_chmod(path, mode)

        monkeypatch.setattr("agent_sec_cli.daemon.server.os.chmod", fail_socket_chmod)

        try:
            await server.start()
        except PermissionError:
            pass
        else:
            raise AssertionError("expected chmod failure during daemon start")

        rebound = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            rebound.bind(str(socket_path))
        finally:
            rebound.close()
            with contextlib.suppress(FileNotFoundError):
                socket_path.unlink()

        return server._server, socket_path.exists()

    bound_server, socket_exists = asyncio.run(scenario())

    assert bound_server is None
    assert socket_exists is False


def test_daemon_server_returns_busy_when_connection_limit_is_reached(tmp_path: Path):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        handler_started = asyncio.Event()
        handler_release = asyncio.Event()

        async def slow_handler(
            _request: DaemonRequest, _runtime: DaemonRuntime
        ) -> HandlerResult:
            handler_started.set()
            await handler_release.wait()
            return HandlerResult(data={"done": True})

        registry = MethodRegistry()
        registry.register(
            MethodSpec(method="slow", handler=slow_handler, lifecycle="test")
        )
        registry.register(
            MethodSpec(
                method="daemon.health",
                handler=lambda _request, _runtime: HandlerResult(data={}),
                lifecycle="admin",
            )
        )
        server = DaemonServer(
            socket_path=socket_path, registry=registry, max_connections=1
        )
        await server.start()
        try:
            first_response_task = asyncio.create_task(
                _send_daemon_request(
                    socket_path,
                    DaemonRequest(method="slow"),
                )
            )
            await asyncio.wait_for(handler_started.wait(), timeout=0.5)

            busy_response = await _send_daemon_request(
                socket_path,
                DaemonRequest(method="daemon.health"),
            )

            handler_release.set()
            first_response = await asyncio.wait_for(first_response_task, timeout=0.5)
        finally:
            await server.stop()

        return busy_response, first_response

    busy_response, first_response = asyncio.run(scenario())

    assert busy_response.ok is False
    _assert_uuid(busy_response.request_id)
    assert busy_response.error is not None
    assert busy_response.error["code"] == "busy"
    assert first_response.ok is True
    _assert_uuid(first_response.request_id)


def test_daemon_server_rejects_oversized_handler_response(tmp_path: Path, caplog):
    caplog.set_level(logging.INFO, logger="agent-sec-core.daemon")

    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"

        async def large_handler(
            _request: DaemonRequest, _runtime: DaemonRuntime
        ) -> HandlerResult:
            return HandlerResult(data={"blob": "x" * 256})

        registry = MethodRegistry()
        registry.register(
            MethodSpec(method="large", handler=large_handler, lifecycle="test")
        )
        server = DaemonServer(
            socket_path=socket_path,
            registry=registry,
            max_response_bytes=320,
        )
        await server.start()
        try:
            response = await _send_daemon_request(
                socket_path,
                DaemonRequest(method="large"),
            )
        finally:
            await server.stop()

        return response

    response = asyncio.run(scenario())

    _assert_uuid(response.request_id)
    assert response.ok is False
    assert response.error is not None
    assert response.error["code"] == "payload_too_large"
    assert response.stderr == "response payload exceeds 320 bytes"

    matching_logs = [
        record
        for record in caplog.records
        if record.name == "agent-sec-core.daemon"
        and getattr(record, "diagnostic_event", None) == "daemon_request_completed"
        and getattr(record, "data", {}).get("request_id") == response.request_id
    ]
    assert matching_logs
    assert matching_logs[-1].data["ok"] is False
    assert matching_logs[-1].data["error_code"] == "payload_too_large"


def test_daemon_server_returns_internal_error_for_unserializable_response(
    tmp_path: Path,
):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"

        async def unserializable_handler(
            _request: DaemonRequest, _runtime: DaemonRuntime
        ) -> HandlerResult:
            return HandlerResult(data={"value": object()})

        registry = MethodRegistry()
        registry.register(
            MethodSpec(
                method="unserializable",
                handler=unserializable_handler,
                lifecycle="test",
            )
        )
        registry.register(
            MethodSpec(
                method="daemon.health",
                handler=lambda _request, _runtime: HandlerResult(data={}),
                lifecycle="admin",
            )
        )
        server = DaemonServer(socket_path=socket_path, registry=registry)
        await server.start()
        try:
            error_response = await _send_daemon_request(
                socket_path,
                DaemonRequest(method="unserializable"),
            )
            health_response = await _send_daemon_request(
                socket_path,
                DaemonRequest(method="daemon.health"),
            )
        finally:
            await server.stop()

        return error_response, health_response

    error_response, health_response = asyncio.run(scenario())

    assert error_response.ok is False
    _assert_uuid(error_response.request_id)
    assert error_response.error is not None
    assert error_response.error["code"] == "internal_error"
    assert error_response.stderr == "daemon internal error"
    assert health_response.ok is True


def test_daemon_server_returns_internal_error_for_unexpected_prepare_failure(
    monkeypatch,
    tmp_path: Path,
):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        server = DaemonServer(socket_path=socket_path)
        original_prepare = server.gateway.prepare

        def fail_prepare_for_request(request: DaemonRequest):
            if request.method == "explode.prepare":
                raise RuntimeError("prepare exploded")
            return original_prepare(request)

        monkeypatch.setattr(server.gateway, "prepare", fail_prepare_for_request)
        await server.start()
        try:
            error_response = await _send_daemon_request(
                socket_path,
                DaemonRequest(method="explode.prepare"),
            )
            health_response = await _send_daemon_request(
                socket_path,
                DaemonRequest(method="daemon.health"),
            )
        finally:
            await server.stop()

        return error_response, health_response

    error_response, health_response = asyncio.run(scenario())

    assert error_response.ok is False
    _assert_uuid(error_response.request_id)
    assert error_response.error is not None
    assert error_response.error["code"] == "internal_error"
    assert error_response.stderr == "daemon internal error"
    assert health_response.ok is True


def test_daemon_server_suppresses_method_access_log(tmp_path: Path, caplog):
    caplog.set_level(logging.INFO, logger="agent-sec-core.daemon")

    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        registry = MethodRegistry()
        registry.register(
            MethodSpec(
                method="quiet",
                handler=lambda _request, _runtime: HandlerResult(data={"ok": True}),
                lifecycle="test",
                access_log=False,
            )
        )
        server = DaemonServer(socket_path=socket_path, registry=registry)
        await server.start()
        try:
            response = await _send_daemon_request(
                socket_path,
                DaemonRequest(method="quiet"),
            )
        finally:
            await server.stop()

        return response

    response = asyncio.run(scenario())

    assert response.ok is True
    _assert_uuid(response.request_id)
    matching_logs = [
        record
        for record in caplog.records
        if record.name == "agent-sec-core.daemon"
        and getattr(record, "data", {}).get("request_id") == response.request_id
    ]
    assert matching_logs == []


def test_idle_request_read_times_out_and_releases_connection(tmp_path: Path):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        server = DaemonServer(
            socket_path=socket_path,
            max_connections=1,
            request_read_timeout_ms=25,
        )
        await server.start()
        try:
            reader, writer = await asyncio.open_unix_connection(str(socket_path))
            timeout_line = await asyncio.wait_for(reader.readline(), timeout=0.5)
            writer.close()
            await writer.wait_closed()

            health_response = await _send_daemon_request(
                socket_path,
                DaemonRequest(method="daemon.health"),
            )
        finally:
            await server.stop()

        return parse_response_line(timeout_line), health_response

    timeout_response, health_response = asyncio.run(scenario())

    assert timeout_response.ok is False
    assert timeout_response.error is not None
    assert timeout_response.error["code"] == "timeout"
    assert timeout_response.stderr == "daemon request timed out after 25 ms"
    _assert_uuid(timeout_response.request_id)
    assert health_response.ok is True
    _assert_uuid(health_response.request_id)


def test_partial_request_read_times_out_and_releases_connection(tmp_path: Path):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        server = DaemonServer(
            socket_path=socket_path,
            max_connections=1,
            request_read_timeout_ms=25,
        )
        await server.start()
        try:
            reader, writer = await asyncio.open_unix_connection(str(socket_path))
            writer.write(b'{"id":"partial"')
            await writer.drain()
            timeout_line = await asyncio.wait_for(reader.readline(), timeout=0.5)
            writer.close()
            await writer.wait_closed()

            health_response = await _send_daemon_request(
                socket_path,
                DaemonRequest(method="daemon.health"),
            )
        finally:
            await server.stop()

        return parse_response_line(timeout_line), health_response

    timeout_response, health_response = asyncio.run(scenario())

    assert timeout_response.ok is False
    assert timeout_response.error is not None
    assert timeout_response.error["code"] == "timeout"
    _assert_uuid(timeout_response.request_id)
    assert health_response.ok is True
    _assert_uuid(health_response.request_id)


def test_bad_request_does_not_steal_concurrent_inflight_counter(tmp_path: Path):
    """Parse-time failures must not decrement another request's inflight slot."""

    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        handler_started = asyncio.Event()
        handler_release = asyncio.Event()
        observed_inflight: list[int] = []

        async def slow_handler(
            _request: DaemonRequest, runtime: DaemonRuntime
        ) -> HandlerResult:
            handler_started.set()
            await handler_release.wait()
            observed_inflight.append(runtime.queues.inflight)
            return HandlerResult(data={"done": True})

        registry = MethodRegistry()
        registry.register(
            MethodSpec(method="slow", handler=slow_handler, lifecycle="test")
        )
        server = DaemonServer(socket_path=socket_path, registry=registry)
        await server.start()
        try:
            slow_task = asyncio.create_task(
                _send_daemon_request(
                    socket_path,
                    DaemonRequest(method="slow"),
                )
            )
            await asyncio.wait_for(handler_started.wait(), timeout=0.5)

            reader, writer = await asyncio.open_unix_connection(str(socket_path))
            writer.write(b"{bad-json}\n")
            await writer.drain()
            bad_line = await reader.readline()
            writer.close()
            await writer.wait_closed()

            bad_response = parse_response_line(bad_line)

            handler_release.set()
            slow_response = await asyncio.wait_for(slow_task, timeout=0.5)
        finally:
            await server.stop()

        return bad_response, slow_response, observed_inflight

    bad_response, slow_response, observed_inflight = asyncio.run(scenario())

    assert bad_response.ok is False
    assert bad_response.error is not None
    assert bad_response.error["code"] == "bad_request"
    _assert_uuid(bad_response.request_id)
    assert slow_response.ok is True
    _assert_uuid(slow_response.request_id)
    assert observed_inflight == [1]


def test_daemon_server_stop_drains_inflight_request(tmp_path: Path):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        handler_started = asyncio.Event()
        handler_release = asyncio.Event()

        async def slow_handler(
            _request: DaemonRequest, _runtime: DaemonRuntime
        ) -> HandlerResult:
            handler_started.set()
            await handler_release.wait()
            return HandlerResult(data={"done": True})

        registry = MethodRegistry()
        registry.register(
            MethodSpec(method="slow", handler=slow_handler, lifecycle="test")
        )
        server = DaemonServer(socket_path=socket_path, registry=registry)
        await server.start()

        response_task = asyncio.create_task(
            _send_daemon_request(
                socket_path,
                DaemonRequest(method="slow"),
            )
        )
        await asyncio.wait_for(handler_started.wait(), timeout=0.5)

        stop_task = asyncio.create_task(server.stop())
        await asyncio.sleep(0.01)
        stop_is_waiting_for_drain = not stop_task.done()
        handler_release.set()
        await asyncio.wait_for(stop_task, timeout=0.5)
        response = await asyncio.wait_for(response_task, timeout=0.5)

        return stop_is_waiting_for_drain, response, socket_path.exists()

    stop_is_waiting_for_drain, response, socket_exists = asyncio.run(scenario())

    assert stop_is_waiting_for_drain is True
    assert response.ok is True
    _assert_uuid(response.request_id)
    assert socket_exists is False


def test_prepare_socket_path_removes_unreachable_stale_socket(tmp_path: Path):
    socket_path = tmp_path / "runtime" / "daemon.sock"
    socket_path.parent.mkdir(mode=0o700)
    stale_socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        stale_socket.bind(str(socket_path))
    finally:
        stale_socket.close()

    lock = prepare_socket_path(socket_path)
    try:
        assert not socket_path.exists()
        assert stat.S_IMODE(socket_path.parent.stat().st_mode) == 0o700
    finally:
        lock.release()


def test_prepare_socket_path_rejects_existing_insecure_runtime_without_chmod(
    tmp_path: Path,
):
    socket_path = tmp_path / "runtime" / "daemon.sock"
    socket_path.parent.mkdir()
    os.chmod(socket_path.parent, 0o755)

    try:
        prepare_socket_path(socket_path)
    except DaemonRuntimePathError as exc:
        assert "must be mode 0700" in exc.message
    else:
        raise AssertionError("expected insecure runtime directory to fail")

    assert stat.S_IMODE(socket_path.parent.stat().st_mode) == 0o755


def test_prepare_socket_path_rejects_relative_socket_parent_without_chmod(
    monkeypatch,
    tmp_path: Path,
):
    project_dir = tmp_path / "project"
    project_dir.mkdir()
    os.chmod(project_dir, 0o755)
    monkeypatch.chdir(project_dir)

    try:
        prepare_socket_path(Path("daemon.sock"))
    except DaemonRuntimePathError as exc:
        assert "must be mode 0700" in exc.message
    else:
        raise AssertionError("expected bare relative socket parent to fail")

    assert stat.S_IMODE(project_dir.stat().st_mode) == 0o755


def test_prepare_socket_path_rejects_symlink_runtime_directory(tmp_path: Path):
    real_runtime = tmp_path / "real-runtime"
    linked_runtime = tmp_path / "linked-runtime"
    real_runtime.mkdir()
    linked_runtime.symlink_to(real_runtime)

    try:
        prepare_socket_path(linked_runtime / "daemon.sock")
    except DaemonRuntimePathError as exc:
        assert "must not be a symlink" in exc.message
    else:
        raise AssertionError("expected symlink runtime directory to fail")


def test_health_does_not_import_heavy_modules(tmp_path: Path):
    heavy_prefixes = (
        "agent_sec_cli.code_scanner",
        "agent_sec_cli.pii_checker",
        "agent_sec_cli.prompt_scanner",
        "agent_sec_cli.security_middleware",
        "agent_sec_cli.skill_ledger",
    )
    before = _matching_modules(heavy_prefixes)

    snapshot = build_health_snapshot(
        DaemonRuntime(socket_path=tmp_path / "daemon.sock")
    )
    registry = create_default_registry()

    assert snapshot["status"] == "ok"
    assert registry.methods() == (
        "daemon.health",
        "obs.runs.list",
        "obs.sessions.list",
        "obs.timeline.get",
        "sec.events.count_by",
        "sec.events.get",
        "sec.events.list",
        "sec.summary",
        "skill_ledger.skillfs_notify_change",
    )
    assert _matching_modules(heavy_prefixes) == before


def test_health_prompt_scan_is_reported_as_untracked(tmp_path: Path):
    """Health must not claim scanner readiness it cannot observe."""
    snapshot = build_health_snapshot(
        DaemonRuntime(socket_path=tmp_path / "daemon.sock")
    )

    prompt_scan = snapshot["prompt_scan"]

    assert prompt_scan["tracked"] is False
    assert prompt_scan["status"] == "unknown"
    assert prompt_scan["loaded"] is None
    assert prompt_scan["model"] is None
    assert prompt_scan["detail"]
    # Legacy PromptScanRuntimeState keys stay present so monitors reading
    # `.prompt_scan.*` keep parsing, but none may imply availability.
    assert set(prompt_scan) == {
        "status",
        "model",
        "loaded",
        "last_error",
        "last_started_at",
        "last_finished_at",
        "tracked",
        "detail",
    }


def test_completion_log_is_emitted_when_inflight_request_is_cancelled(
    tmp_path: Path, caplog
):
    """Cancellation during drain must still flush a completion log line."""

    caplog.set_level(logging.INFO, logger="agent-sec-core.daemon")

    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        handler_started = asyncio.Event()
        hang_forever = asyncio.Event()

        async def hang_handler(
            _request: DaemonRequest, _runtime: DaemonRuntime
        ) -> HandlerResult:
            handler_started.set()
            await hang_forever.wait()
            return HandlerResult(data={})

        registry = MethodRegistry()
        registry.register(
            MethodSpec(
                method="hang",
                handler=hang_handler,
                lifecycle="test",
                timeout_ms=60_000,
            )
        )
        server = DaemonServer(socket_path=socket_path, registry=registry)
        # Force drain to cancel pending tasks immediately instead of waiting.
        server._drain_timeout_seconds = 0.0
        await server.start()

        client_task = asyncio.create_task(
            _send_daemon_request(
                socket_path,
                DaemonRequest(method="hang", timeout_ms=60_000),
            )
        )
        await asyncio.wait_for(handler_started.wait(), timeout=0.5)

        await server.stop()

        with contextlib.suppress(Exception):
            await client_task

    asyncio.run(scenario())

    matching_logs = [
        record
        for record in caplog.records
        if record.name == "agent-sec-core.daemon"
        and getattr(record, "diagnostic_event", None) == "daemon_request_completed"
        and getattr(record, "data", {}).get("method") == "hang"
    ]

    assert matching_logs, "expected completion log for cancelled in-flight request"
    record = matching_logs[-1]
    assert record.diagnostic_event == "daemon_request_completed"
    _assert_uuid(record.data["request_id"])
    assert record.data["method"] == "hang"
    assert record.data["ok"] is False


def test_completion_log_outputs_structured_fields(caplog):
    caplog.set_level(logging.INFO, logger="agent-sec-core.daemon")

    _log_request_completion(
        request_id="req-log",
        method="daemon.health",
        response=success_response("req-log"),
        started=time.monotonic() - 0.01,
        bytes_in=12,
        bytes_out=34,
    )

    record = caplog.records[-1]
    payload = record.data
    assert record.message == "daemon request completed"
    assert record.diagnostic_event == "daemon_request_completed"
    assert payload["request_id"] == "req-log"
    assert payload["method"] == "daemon.health"
    assert payload["ok"] is True
    assert payload["exit_code"] == 0
    assert payload["error_code"] is None
    assert payload["bytes_in"] == 12
    assert payload["bytes_out"] == 34
    assert "latency_ms" in payload


def test_request_started_log_outputs_structured_fields(tmp_path: Path, caplog):
    caplog.set_level(logging.INFO, logger="agent-sec-core.daemon")

    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        registry = MethodRegistry()
        registry.register(
            MethodSpec(
                method="started",
                handler=lambda _request, _runtime: HandlerResult(data={"ok": True}),
                lifecycle="test",
            )
        )
        server = DaemonServer(socket_path=socket_path, registry=registry)
        await server.start()
        try:
            return await _send_daemon_request(
                socket_path,
                DaemonRequest(
                    method="started",
                    caller="cli",
                    trace_context={
                        "trace_id": "trace-started",
                        "session_id": "session-started",
                    },
                ),
            )
        finally:
            await server.stop()

    response = asyncio.run(scenario())

    assert response.ok is True
    _assert_uuid(response.request_id)
    matching_logs = [
        record
        for record in caplog.records
        if record.name == "agent-sec-core.daemon"
        and getattr(record, "diagnostic_event", None) == "daemon_request_started"
        and getattr(record, "data", {}).get("request_id") == response.request_id
    ]
    assert matching_logs
    record = matching_logs[-1]
    assert record.message == "daemon request started"
    assert record.trace_id == "trace-started"
    assert record.session_id == "session-started"
    assert record.data == {
        "request_id": response.request_id,
        "method": "started",
        "caller": "cli",
    }


def test_gateway_preserves_missing_trace_id(tmp_path: Path, caplog):
    caplog.set_level(logging.INFO, logger="agent-sec-core.daemon")
    observed_contexts = []

    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"

        def capture_handler(
            request: DaemonRequest, _runtime: DaemonRuntime
        ) -> HandlerResult:
            observed_contexts.append(
                (request.trace_context, get_current_trace_context())
            )
            return HandlerResult(data={"trace_context": request.trace_context})

        registry = MethodRegistry()
        registry.register(
            MethodSpec(
                method="capture",
                handler=capture_handler,
                lifecycle="test",
            )
        )
        server = DaemonServer(socket_path=socket_path, registry=registry)
        await server.start()
        try:
            return await _send_daemon_request(
                socket_path,
                DaemonRequest(
                    method="capture",
                    trace_context={
                        "session_id": "session-default",
                        "agent_name": "hermes",
                    },
                ),
            )
        finally:
            await server.stop()

    response = asyncio.run(scenario())

    assert response.ok is True
    assert response.data["trace_context"] == {
        "session_id": "session-default",
        "agent_name": "hermes",
    }
    assert observed_contexts == [
        (
            {
                "session_id": "session-default",
                "agent_name": "hermes",
            },
            TraceContext(
                session_id="session-default",
                agent_name="hermes",
            ),
        )
    ]

    matching_logs = [
        record
        for record in caplog.records
        if record.name == "agent-sec-core.daemon"
        and getattr(record, "diagnostic_event", None)
        in {"daemon_request_started", "daemon_request_completed"}
        and getattr(record, "data", {}).get("request_id") == response.request_id
    ]
    assert len(matching_logs) == 2
    assert all(not hasattr(record, "trace_id") for record in matching_logs)
    assert all(record.session_id == "session-default" for record in matching_logs)


def test_completion_log_writes_daemon_jsonl_with_trace_context(
    tmp_path: Path,
    monkeypatch,
):
    monkeypatch.setenv("AGENT_SEC_DAEMON_LOG_LEVEL", "info")
    log_path = tmp_path / "daemon.jsonl"
    logger = logging.getLogger("agent-sec-core.daemon")
    original_level = logger.level
    reset_daemon_diagnostic_logging_for_tests()

    try:
        logger.setLevel(logging.INFO)
        setup_daemon_logging(path=log_path)
        _log_request_completion(
            request_id="req-daemon-jsonl",
            method="scan-prompt",
            caller="cli",
            response=success_response("req-daemon-jsonl"),
            started=time.monotonic() - 0.01,
            bytes_in=56,
            bytes_out=78,
            trace_context=TraceContext(
                trace_id="trace-1",
                session_id="session-1",
                run_id="run-1",
                call_id="call-1",
                tool_call_id="tool-1",
            ),
        )
    finally:
        reset_daemon_diagnostic_logging_for_tests()
        logger.setLevel(original_level)

    payload = json.loads(log_path.read_text(encoding="utf-8").splitlines()[0])
    assert payload["component"] == "daemon"
    assert payload["event"] == "daemon_request_completed"
    assert payload["message"] == "daemon request completed"
    assert payload["pid"] == os.getpid()
    assert payload["trace_id"] == "trace-1"
    assert payload["session_id"] == "session-1"
    assert payload["run_id"] == "run-1"
    assert payload["call_id"] == "call-1"
    assert payload["tool_call_id"] == "tool-1"
    assert "invocation_id" not in payload
    assert payload["request_id"] == "req-daemon-jsonl"
    assert payload["data"]["request_id"] == "req-daemon-jsonl"
    assert payload["data"]["method"] == "scan-prompt"
    assert payload["data"]["caller"] == "cli"
    assert payload["data"]["ok"] is True
    assert payload["data"]["exit_code"] == 0
    assert payload["data"]["error_code"] is None
    assert "latency_ms" in payload["data"]
    assert payload["data"]["queue_ms"] == 0
    assert payload["data"]["bytes_in"] == 56
    assert payload["data"]["bytes_out"] == 78


def test_daemon_log_level_off_disables_daemon_jsonl(
    tmp_path: Path,
    monkeypatch,
):
    monkeypatch.setenv("AGENT_SEC_DAEMON_LOG_LEVEL", "off")
    log_path = tmp_path / "daemon.jsonl"
    logger = logging.getLogger("agent-sec-core.daemon")
    original_level = logger.level
    reset_daemon_diagnostic_logging_for_tests()

    try:
        logger.setLevel(logging.INFO)
        setup_daemon_logging(path=log_path)
        _log_request_completion(
            request_id="req-daemon-off",
            method="scan-prompt",
            response=success_response("req-daemon-off"),
            started=time.monotonic() - 0.01,
            bytes_in=12,
            bytes_out=34,
        )
    finally:
        reset_daemon_diagnostic_logging_for_tests()
        logger.setLevel(original_level)

    assert not log_path.exists()


def test_daemon_logging_captures_third_party_logs(
    tmp_path: Path,
    monkeypatch,
):
    monkeypatch.setenv("AGENT_SEC_DAEMON_LOG_LEVEL", "info")
    log_path = tmp_path / "daemon.jsonl"
    root_logger = logging.getLogger()
    third_party_logger = logging.getLogger("third_party.daemon_dependency")
    original_root_level = root_logger.level
    original_third_party_level = third_party_logger.level
    original_third_party_propagate = third_party_logger.propagate
    reset_daemon_diagnostic_logging_for_tests()

    try:
        root_logger.setLevel(logging.DEBUG)
        third_party_logger.setLevel(logging.NOTSET)
        third_party_logger.propagate = True
        setup_daemon_logging(path=log_path)

        with daemon_request_context(
            TraceContext(
                trace_id="trace-third-party",
                session_id="session-third-party",
                run_id="run-third-party",
            ),
            request_id="req-third-party",
        ):
            third_party_logger.info("dependency ready")
        third_party_logger.info("outside request")
    finally:
        reset_daemon_diagnostic_logging_for_tests()
        root_logger.setLevel(original_root_level)
        third_party_logger.setLevel(original_third_party_level)
        third_party_logger.propagate = original_third_party_propagate

    lines = log_path.read_text(encoding="utf-8").splitlines()
    payload = json.loads(lines[0])
    assert payload["component"] == "daemon"
    assert payload["event"] == "daemon_log"
    assert payload["logger"] == "third_party.daemon_dependency"
    assert payload["message"] == "dependency ready"
    assert payload["request_id"] == "req-third-party"
    assert payload["trace_id"] == "trace-third-party"
    assert payload["session_id"] == "session-third-party"
    assert payload["run_id"] == "run-third-party"
    outside_payload = json.loads(lines[1])
    assert outside_payload["message"] == "outside request"
    assert "request_id" not in outside_payload


def test_gateway_request_context_adds_request_id_to_ordinary_daemon_logs(
    tmp_path: Path,
    monkeypatch,
):
    monkeypatch.setenv("AGENT_SEC_DAEMON_LOG_LEVEL", "info")
    log_path = tmp_path / "daemon.jsonl"
    root_logger = logging.getLogger()
    handler_logger = logging.getLogger("third_party.gateway_dependency")
    original_root_level = root_logger.level
    original_handler_level = handler_logger.level
    original_handler_propagate = handler_logger.propagate
    reset_daemon_diagnostic_logging_for_tests()

    async def scenario() -> DaemonResponse:
        socket_path = tmp_path / "runtime" / "daemon.sock"

        def logging_handler(
            _request: DaemonRequest,
            _runtime: DaemonRuntime,
        ) -> HandlerResult:
            handler_logger.info("inside gateway request")
            return HandlerResult(data={"ok": True})

        registry = MethodRegistry()
        registry.register(
            MethodSpec(method="log-inside", handler=logging_handler, lifecycle="test")
        )
        server = DaemonServer(socket_path=socket_path, registry=registry)
        await server.start()
        try:
            return await _send_daemon_request(
                socket_path,
                DaemonRequest(method="log-inside"),
            )
        finally:
            await server.stop()

    try:
        root_logger.setLevel(logging.DEBUG)
        handler_logger.setLevel(logging.NOTSET)
        handler_logger.propagate = True
        setup_daemon_logging(path=log_path)

        response = asyncio.run(scenario())
    finally:
        reset_daemon_diagnostic_logging_for_tests()
        root_logger.setLevel(original_root_level)
        handler_logger.setLevel(original_handler_level)
        handler_logger.propagate = original_handler_propagate

    assert response.ok is True
    _assert_uuid(response.request_id)
    payloads = [
        json.loads(line) for line in log_path.read_text(encoding="utf-8").splitlines()
    ]
    matching_payloads = [
        payload
        for payload in payloads
        if payload.get("message") == "inside gateway request"
    ]
    assert matching_payloads
    assert matching_payloads[-1]["request_id"] == response.request_id


def test_setup_daemon_logging_does_not_mutate_daemon_logger_level(
    tmp_path: Path,
    monkeypatch,
):
    monkeypatch.setenv("AGENT_SEC_DAEMON_LOG_LEVEL", "debug")
    logger = logging.getLogger("agent-sec-core.daemon")
    original_level = logger.level
    logger.setLevel(logging.ERROR)
    reset_daemon_diagnostic_logging_for_tests()

    try:
        setup_daemon_logging(path=tmp_path / "daemon.jsonl")
        assert logger.level == logging.ERROR
    finally:
        reset_daemon_diagnostic_logging_for_tests()
        logger.setLevel(original_level)


def test_unknown_method_completion_log_preserves_trace_context(
    tmp_path: Path,
    caplog,
):
    caplog.set_level(logging.INFO, logger="agent-sec-core.daemon")

    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        server = DaemonServer(socket_path=socket_path, registry=MethodRegistry())
        await server.start()
        try:
            return await _send_daemon_request(
                socket_path,
                DaemonRequest(
                    method="unknown.method",
                    trace_context={
                        "trace_id": "trace-unknown",
                        "session_id": "session-unknown",
                    },
                ),
            )
        finally:
            await server.stop()

    response = asyncio.run(scenario())

    assert response.ok is False
    assert response.error is not None
    assert response.error["code"] == "unknown_method"

    matching_logs = [
        record
        for record in caplog.records
        if record.name == "agent-sec-core.daemon"
        and getattr(record, "diagnostic_event", None) == "daemon_request_completed"
        and getattr(record, "data", {}).get("request_id") == response.request_id
    ]
    assert matching_logs
    record = matching_logs[-1]
    assert record.trace_id == "trace-unknown"
    assert record.session_id == "session-unknown"
    assert record.data["method"] == "unknown.method"


def test_authenticated_notify_dispatches_and_returns_signed_coalesced_response(
    monkeypatch,
    tmp_path: Path,
):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        secret = _configure_notify_auth(monkeypatch, tmp_path)
        dispatches = []
        server = DaemonServer(
            socket_path=socket_path,
            registry=_tracking_notify_registry(dispatches),
        )
        await server.start()
        try:
            reader, writer, session = await _open_authenticated_notify_connection(
                socket_path,
                secret,
            )
            try:
                # A single write deliberately coalesces the business payload and tag.
                writer.write(
                    session.protect_payload(_notify_request_payload(), "client")
                )
                await writer.drain()
                response = await _read_authenticated_response(reader, session)
            finally:
                await _close_stream_writer(writer)
        finally:
            await server.stop()
        return response, dispatches

    response, dispatches = asyncio.run(scenario())

    assert response.ok is True
    assert response.data == {"schemaVersion": 2, "accepted": True}
    assert dispatches == [METHOD_SKILLFS_NOTIFY_CHANGE]


def test_authenticated_business_bad_request_response_is_signed(
    monkeypatch,
    tmp_path: Path,
):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        secret = _configure_notify_auth(monkeypatch, tmp_path)
        dispatches = []
        server = DaemonServer(
            socket_path=socket_path,
            registry=_tracking_notify_registry(dispatches),
        )
        await server.start()
        try:
            reader, writer, session = await _open_authenticated_notify_connection(
                socket_path,
                secret,
            )
            try:
                writer.write(session.protect_payload(b'{"method":', "client"))
                await writer.drain()
                response = await _read_authenticated_response(reader, session)
            finally:
                await _close_stream_writer(writer)
        finally:
            await server.stop()
        return response, dispatches

    response, dispatches = asyncio.run(scenario())

    assert response.ok is False
    assert response.error is not None
    assert response.error["code"] == "bad_request"
    assert dispatches == []


def test_authenticated_notify_wrong_key_closes_without_dispatch(
    monkeypatch,
    tmp_path: Path,
):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        _configure_notify_auth(monkeypatch, tmp_path, value=b"s" * 32)
        dispatches = []
        server = DaemonServer(
            socket_path=socket_path,
            registry=_tracking_notify_registry(dispatches),
        )
        await server.start()
        try:
            reader, writer = await asyncio.open_unix_connection(str(socket_path))
            try:
                writer.write(build_auth_init_frame())
                await writer.drain()
                challenge = await _read_frame(reader)
                nonce = parse_auth_challenge_frame(challenge)
                writer.write(
                    build_auth_proof_frame(
                        "auth.proof",
                        SharedSecret(b"w" * 32),
                        NOTIFY_CLIENT_DOMAIN,
                        nonce,
                    )
                )
                await writer.drain()
                remainder = await asyncio.wait_for(reader.read(), timeout=0.5)
            finally:
                await _close_stream_writer(writer)
        finally:
            await server.stop()
        return remainder, dispatches

    remainder, dispatches = asyncio.run(scenario())

    assert remainder == b""
    assert dispatches == []


def test_authenticated_notify_slow_handshake_closes_without_dispatch(
    monkeypatch,
    tmp_path: Path,
):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        _configure_notify_auth(monkeypatch, tmp_path)
        dispatches = []
        server = DaemonServer(
            socket_path=socket_path,
            registry=_tracking_notify_registry(dispatches),
            request_read_timeout_ms=100,
        )
        await server.start()
        try:
            reader, writer = await asyncio.open_unix_connection(str(socket_path))
            try:
                writer.write(build_auth_init_frame())
                await writer.drain()
                parse_auth_challenge_frame(await _read_frame(reader))
                writer.write(b'{"authVersion":"1","type":"auth.proof"')
                await writer.drain()
                remainder = await asyncio.wait_for(reader.read(), timeout=0.5)
            finally:
                await _close_stream_writer(writer)
        finally:
            await server.stop()
        return remainder, dispatches

    remainder, dispatches = asyncio.run(scenario())

    assert remainder == b""
    assert dispatches == []


def test_authenticated_notify_slow_init_uses_handshake_deadline(
    monkeypatch,
    tmp_path: Path,
):
    monkeypatch.setattr(
        daemon_server_module,
        "AUTH_HANDSHAKE_TIMEOUT_SECONDS",
        0.05,
    )

    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        _configure_notify_auth(monkeypatch, tmp_path)
        dispatches = []
        server = DaemonServer(
            socket_path=socket_path,
            registry=_tracking_notify_registry(dispatches),
            request_read_timeout_ms=1000,
        )
        await server.start()
        try:
            reader, writer = await asyncio.open_unix_connection(str(socket_path))
            try:
                writer.write(b'{"authVersion":"1"')
                await writer.drain()
                remainder = await asyncio.wait_for(reader.read(), timeout=0.5)
            finally:
                await _close_stream_writer(writer)
        finally:
            await server.stop()
        return remainder, dispatches

    remainder, dispatches = asyncio.run(scenario())

    assert remainder == b""
    assert dispatches == []


def test_authenticated_notify_tamper_closes_without_dispatch(
    monkeypatch,
    tmp_path: Path,
):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        secret = _configure_notify_auth(monkeypatch, tmp_path)
        dispatches = []
        server = DaemonServer(
            socket_path=socket_path,
            registry=_tracking_notify_registry(dispatches),
        )
        await server.start()
        try:
            reader, writer, session = await _open_authenticated_notify_connection(
                socket_path,
                secret,
            )
            try:
                protected = session.protect_payload(_notify_request_payload(), "client")
                payload, auth_frame = protected.split(b"\n", maxsplit=1)
                tampered = payload.replace(b'"write"', b'"rename"')
                assert tampered != payload
                writer.write(tampered + b"\n" + auth_frame)
                await writer.drain()
                remainder = await asyncio.wait_for(reader.read(), timeout=0.5)
            finally:
                await _close_stream_writer(writer)
        finally:
            await server.stop()
        return remainder, dispatches

    remainder, dispatches = asyncio.run(scenario())

    assert remainder == b""
    assert dispatches == []


def test_authenticated_notify_eof_closes_without_dispatch(
    monkeypatch,
    tmp_path: Path,
):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        secret = _configure_notify_auth(monkeypatch, tmp_path)
        dispatches = []
        server = DaemonServer(
            socket_path=socket_path,
            registry=_tracking_notify_registry(dispatches),
        )
        await server.start()
        try:
            reader, writer, _session = await _open_authenticated_notify_connection(
                socket_path,
                secret,
            )
            try:
                writer.write(b'{"method":"skill_ledger.skillfs_notify_change"}')
                await writer.drain()
                assert writer.can_write_eof()
                writer.write_eof()
                await writer.drain()
                remainder = await asyncio.wait_for(reader.read(), timeout=0.5)
            finally:
                await _close_stream_writer(writer)
        finally:
            await server.stop()
        return remainder, dispatches

    remainder, dispatches = asyncio.run(scenario())

    assert remainder == b""
    assert dispatches == []


def test_authenticated_notify_oversize_closes_without_dispatch(
    monkeypatch,
    tmp_path: Path,
):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        secret = _configure_notify_auth(monkeypatch, tmp_path)
        dispatches = []
        max_request_bytes = 128
        server = DaemonServer(
            socket_path=socket_path,
            registry=_tracking_notify_registry(dispatches),
            max_request_bytes=max_request_bytes,
        )
        await server.start()
        try:
            reader, writer, session = await _open_authenticated_notify_connection(
                socket_path,
                secret,
            )
            try:
                writer.write(
                    session.protect_payload(b"x" * max_request_bytes, "client")
                )
                await writer.drain()
                remainder = await asyncio.wait_for(reader.read(), timeout=0.5)
            finally:
                await _close_stream_writer(writer)
        finally:
            await server.stop()
        return remainder, dispatches

    remainder, dispatches = asyncio.run(scenario())

    assert remainder == b""
    assert dispatches == []


def test_auth_init_without_key_and_invalid_auth_close_without_dispatch(
    monkeypatch,
    tmp_path: Path,
):
    monkeypatch.delenv(AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE, raising=False)

    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        dispatches = []
        server = DaemonServer(
            socket_path=socket_path,
            registry=_tracking_notify_registry(dispatches),
        )
        await server.start()
        try:
            remainders = []
            for frame in (
                build_auth_init_frame(),
                b'{"authVersion":"1","type":"auth.init","extra":true}\n',
                b'{"method":"daemon.health","ty\\u0070e":"auth.init",' b'"type":0}\n',
            ):
                reader, writer = await asyncio.open_unix_connection(str(socket_path))
                try:
                    writer.write(frame)
                    await writer.drain()
                    remainders.append(
                        await asyncio.wait_for(reader.read(), timeout=0.5)
                    )
                finally:
                    await _close_stream_writer(writer)
        finally:
            await server.stop()
        return remainders, dispatches

    remainders, dispatches = asyncio.run(scenario())

    assert remainders == [b"", b"", b""]
    assert dispatches == []


def test_notify_auth_first_frame_uses_auth_limit_and_fails_closed(
    monkeypatch,
    tmp_path: Path,
):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        _configure_notify_auth(monkeypatch, tmp_path)
        dispatches = []
        server = DaemonServer(
            socket_path=socket_path,
            registry=_tracking_notify_registry(dispatches),
            max_request_bytes=32,
        )
        await server.start()
        try:
            reader, writer = await asyncio.open_unix_connection(str(socket_path))
            try:
                writer.write(build_auth_init_frame())
                await writer.drain()
                challenge = await _read_frame(reader)
                parse_auth_challenge_frame(challenge)
            finally:
                await _close_stream_writer(writer)

            reader, writer = await asyncio.open_unix_connection(str(socket_path))
            try:
                oversized = b'{"authVersion":"' + (b"x" * 4096) + b'"}\n'
                writer.write(oversized)
                await writer.drain()
                remainder = await asyncio.wait_for(reader.read(), timeout=0.5)
            finally:
                await _close_stream_writer(writer)
        finally:
            await server.stop()
        return remainder, dispatches

    remainder, dispatches = asyncio.run(scenario())

    assert remainder == b""
    assert dispatches == []


def test_notify_key_rejects_plain_notify_but_keeps_plain_health(
    monkeypatch,
    tmp_path: Path,
):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        _configure_notify_auth(monkeypatch, tmp_path)
        dispatches = []
        server = DaemonServer(
            socket_path=socket_path,
            registry=_tracking_notify_registry(dispatches, include_health=True),
        )
        await server.start()
        try:
            reader, writer = await asyncio.open_unix_connection(str(socket_path))
            try:
                writer.write(_notify_request_payload() + b"\n")
                await writer.drain()
                plain_notify_response = await asyncio.wait_for(
                    reader.read(), timeout=0.5
                )
            finally:
                await _close_stream_writer(writer)

            health_response = await _send_daemon_request(
                socket_path,
                DaemonRequest(method="daemon.health"),
            )
        finally:
            await server.stop()
        return plain_notify_response, health_response, dispatches

    plain_notify_response, health_response, dispatches = asyncio.run(scenario())

    assert plain_notify_response == b""
    assert health_response.ok is True
    assert health_response.data == {"status": "ok"}
    assert dispatches == ["daemon.health"]


def test_authenticated_non_notify_returns_signed_bad_request(
    monkeypatch,
    tmp_path: Path,
):
    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        secret = _configure_notify_auth(monkeypatch, tmp_path)
        dispatches = []
        server = DaemonServer(
            socket_path=socket_path,
            registry=_tracking_notify_registry(dispatches, include_health=True),
        )
        await server.start()
        try:
            reader, writer, session = await _open_authenticated_notify_connection(
                socket_path,
                secret,
            )
            try:
                health_payload = serialize_request(
                    DaemonRequest(method="daemon.health")
                )[:-1]
                writer.write(session.protect_payload(health_payload, "client"))
                await writer.drain()
                response = await _read_authenticated_response(reader, session)
            finally:
                await _close_stream_writer(writer)
        finally:
            await server.stop()
        return response, dispatches

    response, dispatches = asyncio.run(scenario())

    assert response.ok is False
    assert response.error is not None
    assert response.error["code"] == "bad_request"
    assert METHOD_SKILLFS_NOTIFY_CHANGE in response.error["message"]
    assert dispatches == []


def test_invalid_notify_key_fails_before_bind_or_job_start(
    monkeypatch,
    tmp_path: Path,
):
    key_path = tmp_path / "short.key"
    key_path.write_bytes(b"short")
    key_path.chmod(0o600)
    monkeypatch.setenv(AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE, str(key_path))

    async def scenario():
        socket_path = tmp_path / "runtime" / "daemon.sock"
        server = DaemonServer(socket_path=socket_path)

        with pytest.raises(SkillFsPeerAuthError, match="between 32 and 4096 bytes"):
            await server.start()

        return (
            server._server,
            server._lock,
            server.runtime.jobs.started,
            socket_path.exists(),
        )

    bound_server, lock, jobs_started, socket_exists = asyncio.run(scenario())

    assert bound_server is None
    assert lock is None
    assert jobs_started is False
    assert socket_exists is False


async def _send_daemon_request(
    socket_path: Path,
    request: DaemonRequest,
) -> DaemonResponse:
    reader, writer = await asyncio.open_unix_connection(str(socket_path))
    writer.write(serialize_request(request))
    await writer.drain()
    line = await reader.readline()
    writer.close()
    await writer.wait_closed()
    return parse_response_line(line)


def _configure_notify_auth(
    monkeypatch,
    tmp_path: Path,
    *,
    value: bytes = b"n" * 32,
) -> SharedSecret:
    key_path = tmp_path / "notify.key"
    key_path.write_bytes(value)
    key_path.chmod(0o600)
    monkeypatch.setenv(AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE, str(key_path))
    return SharedSecret(value)


def _tracking_notify_registry(
    dispatches: list[str],
    *,
    include_health: bool = False,
) -> MethodRegistry:
    registry = MethodRegistry()

    def notify_handler(
        request: DaemonRequest,
        _runtime: DaemonRuntime,
    ) -> HandlerResult:
        dispatches.append(request.method)
        return HandlerResult(data={"schemaVersion": 2, "accepted": True})

    registry.register(
        MethodSpec(
            method=METHOD_SKILLFS_NOTIFY_CHANGE,
            handler=notify_handler,
            lifecycle="skill_ledger",
        )
    )
    if include_health:

        def health_handler(
            request: DaemonRequest,
            _runtime: DaemonRuntime,
        ) -> HandlerResult:
            dispatches.append(request.method)
            return HandlerResult(data={"status": "ok"})

        registry.register(
            MethodSpec(
                method="daemon.health",
                handler=health_handler,
                lifecycle="admin",
            )
        )
    return registry


def _notify_request_payload() -> bytes:
    return json.dumps(
        {
            "id": "skillfs-test",
            "method": METHOD_SKILLFS_NOTIFY_CHANGE,
            "params": {
                "schemaVersion": 2,
                "canonicalSkillDir": "/srv/skills/demo",
                "skillId": "demo",
                "eventKind": "write",
                "paths": [".skill-meta/activation.json"],
            },
            "trace_context": {},
            "timeout_ms": 5000,
        },
        separators=(",", ":"),
    ).encode("utf-8")


async def _read_frame(reader: asyncio.StreamReader) -> bytes:
    return await asyncio.wait_for(reader.readline(), timeout=0.5)


async def _open_authenticated_notify_connection(
    socket_path: Path,
    secret: SharedSecret,
) -> tuple[asyncio.StreamReader, asyncio.StreamWriter, AuthenticatedSession]:
    reader, writer = await asyncio.open_unix_connection(str(socket_path))
    writer.write(build_auth_init_frame())
    await writer.drain()

    nonce = parse_auth_challenge_frame(await _read_frame(reader))
    writer.write(
        build_auth_proof_frame(
            "auth.proof",
            secret,
            NOTIFY_CLIENT_DOMAIN,
            nonce,
        )
    )
    await writer.drain()
    verify_auth_proof_frame(
        await _read_frame(reader),
        expected_kind="auth.ok",
        secret=secret,
        domain=NOTIFY_SERVER_DOMAIN,
        nonce=nonce,
    )
    return (
        reader,
        writer,
        AuthenticatedSession(
            secret,
            nonce,
            NOTIFY_CLIENT_DOMAIN,
            NOTIFY_SERVER_DOMAIN,
        ),
    )


async def _read_authenticated_response(
    reader: asyncio.StreamReader,
    session: AuthenticatedSession,
) -> DaemonResponse:
    protected = await asyncio.wait_for(reader.read(), timeout=0.5)
    payload, auth_frame, trailing = protected.split(b"\n", maxsplit=2)
    assert trailing == b""
    session.verify_payload(payload, auth_frame + b"\n", "server")
    return parse_response_line(payload)


async def _close_stream_writer(writer: asyncio.StreamWriter) -> None:
    writer.close()
    with contextlib.suppress(
        ConnectionError,
        BrokenPipeError,
        OSError,
    ):
        await writer.wait_closed()
