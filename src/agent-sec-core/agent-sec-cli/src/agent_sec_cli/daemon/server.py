"""Async Unix socket server for the agent-sec daemon."""

import argparse
import asyncio
import contextlib
import fcntl
import logging
import os
import signal
import stat
import time
from pathlib import Path
from typing import Any, Sequence

from agent_sec_cli.daemon.client import daemon_health_reachable
from agent_sec_cli.daemon.errors import (
    BadRequestError,
    BusyError,
    DaemonAlreadyRunningError,
    DaemonError,
    DaemonRuntimePathError,
    DaemonTimeoutError,
    InternalDaemonError,
    PayloadTooLargeError,
    ResponseTooLargeError,
    ShutdownError,
)
from agent_sec_cli.daemon.gateway import (
    DaemonGateway,
    PreparedDaemonRequest,
    _log_request_completion,
)
from agent_sec_cli.daemon.handlers.security_query import (
    register_security_query_methods,
)
from agent_sec_cli.daemon.handlers.skill_ledger import (
    METHOD_SKILLFS_NOTIFY_CHANGE,
    register_skill_ledger_methods,
)
from agent_sec_cli.daemon.health import register_health_methods
from agent_sec_cli.daemon.jobs.registry import register_default_jobs
from agent_sec_cli.daemon.logging import log_daemon_event, setup_daemon_logging
from agent_sec_cli.daemon.protocol import (
    DEFAULT_MAX_REQUEST_BYTES,
    DEFAULT_MAX_RESPONSE_BYTES,
    DaemonResponse,
    error_response,
    generate_request_id,
    parse_request_line,
    serialize_response,
)
from agent_sec_cli.daemon.registry import MethodRegistry
from agent_sec_cli.daemon.runtime import (
    DaemonRuntime,
    ensure_runtime_directory,
    lock_path_for_socket,
    resolve_socket_path,
)
from agent_sec_cli.skill_ledger.skillfs_peer_auth import (
    AUTH_FRAME_LIMIT,
    AUTH_HANDSHAKE_TIMEOUT_SECONDS,
    NOTIFY_CLIENT_DOMAIN,
    NOTIFY_SERVER_DOMAIN,
    AuthenticatedSession,
    SharedSecret,
    SkillFsPeerAuthError,
    build_auth_challenge_frame,
    build_auth_proof_frame,
    classify_initial_frame,
    configured_notify_key_path,
    verify_auth_proof_frame,
)

LOGGER = logging.getLogger("agent-sec-core.daemon")
DEFAULT_MAX_CONNECTIONS = 64
DEFAULT_DRAIN_TIMEOUT_SECONDS = 2.0
DEFAULT_REQUEST_READ_TIMEOUT_MS = 5000
SocketIdentity = tuple[int, int]
_AUTH_WIRE_FRAME_LIMIT = AUTH_FRAME_LIMIT + 1


class _SilentClose(Exception):
    """Close an unauthenticated or invalid authenticated connection quietly."""


class _ConnectionFrameReader:
    """Read bounded NDJSON frames while retaining coalesced trailing frames."""

    def __init__(self, reader: asyncio.StreamReader) -> None:
        self._reader = reader
        self._buffer = bytearray()

    async def read_frame(
        self,
        max_frame_bytes_including_delimiter: int,
        *,
        deadline: float | None,
        allow_eof_terminated: bool,
    ) -> bytes:
        """Read one frame with a wire-size limit that includes the delimiter."""
        while True:
            newline_index = self._buffer.find(b"\n")
            if newline_index >= 0:
                frame_size = newline_index + 1
                if frame_size > max_frame_bytes_including_delimiter:
                    raise PayloadTooLargeError(max_frame_bytes_including_delimiter)
                frame = bytes(self._buffer[:frame_size])
                del self._buffer[:frame_size]
                return frame

            if len(self._buffer) > max_frame_bytes_including_delimiter:
                raise PayloadTooLargeError(max_frame_bytes_including_delimiter)

            chunk = await self._read_chunk(deadline)
            if chunk:
                self._buffer.extend(chunk)
                continue

            if self._buffer and allow_eof_terminated:
                frame = bytes(self._buffer)
                self._buffer.clear()
                return frame
            if self._buffer:
                raise BadRequestError("incomplete daemon request")
            raise BadRequestError("empty daemon request")

    async def _read_chunk(self, deadline: float | None) -> bytes:
        if deadline is None:
            return await self._reader.read(4096)

        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise asyncio.TimeoutError
        return await asyncio.wait_for(self._reader.read(4096), timeout=remaining)


def create_default_registry() -> MethodRegistry:
    """Create the default daemon method registry."""
    registry = MethodRegistry()
    register_health_methods(registry)
    register_skill_ledger_methods(registry)
    register_security_query_methods(registry)
    return registry


class SingleInstanceLock:
    """Non-blocking file lock for one daemon instance per runtime directory."""

    def __init__(self, lock_path: Path) -> None:
        self.lock_path = lock_path
        self._fd: int | None = None

    def acquire(self) -> None:
        """Acquire the daemon lock or raise if another instance owns it."""
        flags = os.O_CREAT | os.O_RDWR | getattr(os, "O_CLOEXEC", 0)
        fd = os.open(self.lock_path, flags, 0o600)
        os.set_inheritable(fd, False)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            os.close(fd)
            raise DaemonAlreadyRunningError(
                "agent-sec daemon lock is already held"
            ) from exc

        os.ftruncate(fd, 0)
        os.write(fd, str(os.getpid()).encode("ascii"))
        self._fd = fd

    def release(self) -> None:
        """Release the daemon lock."""
        if self._fd is None:
            return

        fcntl.flock(self._fd, fcntl.LOCK_UN)
        os.close(self._fd)
        self._fd = None


class DaemonServer:
    """One-request-per-connection NDJSON Unix socket server."""

    def __init__(
        self,
        socket_path: str | Path | None = None,
        registry: MethodRegistry | None = None,
        max_request_bytes: int = DEFAULT_MAX_REQUEST_BYTES,
        max_response_bytes: int = DEFAULT_MAX_RESPONSE_BYTES,
        max_connections: int = DEFAULT_MAX_CONNECTIONS,
        request_read_timeout_ms: int = DEFAULT_REQUEST_READ_TIMEOUT_MS,
    ) -> None:
        if request_read_timeout_ms <= 0:
            raise ValueError("request_read_timeout_ms must be positive")

        resolved_socket_path = resolve_socket_path(socket_path)
        self.socket_path = resolved_socket_path
        self.registry = create_default_registry() if registry is None else registry
        self.max_request_bytes = max_request_bytes
        self.max_response_bytes = max_response_bytes
        self.max_connections = max_connections
        self.request_read_timeout_ms = request_read_timeout_ms
        self.runtime = DaemonRuntime(socket_path=resolved_socket_path)
        register_default_jobs(self.runtime.jobs)
        self.gateway = DaemonGateway(self.registry, self.runtime)
        self._server: asyncio.Server | None = None
        self._lock: SingleInstanceLock | None = None
        self._active_connections = 0
        self._client_tasks: set[asyncio.Task[None]] = set()
        self._drain_timeout_seconds = DEFAULT_DRAIN_TIMEOUT_SECONDS
        self._previous_umask: int | None = None
        self._socket_identity: SocketIdentity | None = None
        self._notify_auth_secret: SharedSecret | None = None

    async def start(self) -> None:
        """Prepare runtime paths, bind the Unix socket, and start jobs."""
        notify_key_path = configured_notify_key_path()
        self._notify_auth_secret = (
            None if notify_key_path is None else SharedSecret.load(notify_key_path)
        )
        self._set_daemon_umask()
        try:
            self._lock = prepare_socket_path(self.socket_path)
        except Exception:
            self._restore_umask()
            raise

        try:
            await self.runtime.jobs.start_all()
            self._server = await asyncio.start_unix_server(
                self._handle_client,
                path=str(self.socket_path),
            )
            os.chmod(self.socket_path, 0o600)
            self._socket_identity = _socket_identity(self.socket_path)
        except Exception:
            await self._close_server()
            await self.runtime.jobs.stop_all()
            _unlink_socket_if_owned(self.socket_path, self._socket_identity)
            if self._lock is not None:
                self._lock.release()
                self._lock = None
            self._restore_umask()
            raise

    async def serve_forever(self) -> None:
        """Start the daemon and serve requests until cancelled."""
        await self.start()
        if self._server is None:
            raise DaemonRuntimePathError("daemon server failed to start")

        try:
            async with self._server:
                await self._server.serve_forever()
        except asyncio.CancelledError:
            raise
        finally:
            await self.stop()

    async def stop(self) -> None:
        """Stop accepting requests and release daemon resources."""
        self.runtime.mark_stopping()
        await self._close_server()

        await self._drain_client_tasks()
        await self.runtime.jobs.stop_all()
        _unlink_socket_if_owned(self.socket_path, self._socket_identity)
        self._socket_identity = None

        if self._lock is not None:
            self._lock.release()
            self._lock = None
        self._restore_umask()

    async def _handle_client(
        self,
        reader: asyncio.StreamReader,
        writer: asyncio.StreamWriter,
    ) -> None:
        task = asyncio.current_task()
        if task is not None:
            self._client_tasks.add(task)

        if self.runtime.status == "stopping":
            await self._write_immediate_error(writer, ShutdownError())
            if task is not None:
                self._client_tasks.discard(task)
            return

        if self._active_connections >= self.max_connections:
            await self._write_immediate_error(writer, BusyError())
            if task is not None:
                self._client_tasks.discard(task)
            return

        self._active_connections += 1
        try:
            await self._process_client(reader, writer)
        finally:
            self._active_connections -= 1
            if task is not None:
                self._client_tasks.discard(task)

    async def _close_server(self) -> None:
        if self._server is None:
            return

        self._server.close()
        await self._server.wait_closed()
        self._server = None

    async def _process_client(
        self,
        reader: asyncio.StreamReader,
        writer: asyncio.StreamWriter,
    ) -> None:
        fallback_request_id = generate_request_id()
        bytes_in = 0
        bytes_out = 0
        started = time.monotonic()
        response: DaemonResponse | None = None
        prepared_request: PreparedDaemonRequest | None = None
        parsed_request_method: str | None = None
        authenticated_session: AuthenticatedSession | None = None
        silent_close = False
        frame_reader = _ConnectionFrameReader(reader)

        try:
            request_timeout_seconds = self.request_read_timeout_ms / 1000
            request_deadline = started + request_timeout_seconds
            auth_deadline = started + min(
                request_timeout_seconds,
                AUTH_HANDSHAKE_TIMEOUT_SECONDS,
            )
            initial_deadline = (
                auth_deadline
                if self._notify_auth_secret is not None
                else request_deadline
            )
            # A partial first frame cannot yet be classified safely, so a
            # configured peer key bounds that pre-authentication hold time.
            try:
                initial_frame = await frame_reader.read_frame(
                    max(self.max_request_bytes, _AUTH_WIRE_FRAME_LIMIT),
                    deadline=initial_deadline,
                    allow_eof_terminated=True,
                )
            except (asyncio.TimeoutError, DaemonError) as exc:
                if self._notify_auth_secret is not None:
                    raise _SilentClose from exc
                raise

            bytes_in = len(initial_frame)
            try:
                is_auth_init = classify_initial_frame(initial_frame)
            except SkillFsPeerAuthError as exc:
                raise _SilentClose from exc

            if is_auth_init:
                if self._notify_auth_secret is None:
                    raise _SilentClose
                try:
                    (
                        authenticated_session,
                        handshake_bytes_in,
                        handshake_bytes_out,
                    ) = await self._authenticate_notify_client(
                        frame_reader,
                        writer,
                        self._notify_auth_secret,
                        auth_deadline,
                    )
                    bytes_in += handshake_bytes_in
                    bytes_out += handshake_bytes_out
                    request_line, protected_bytes_in = (
                        await self._read_authenticated_request(
                            frame_reader,
                            authenticated_session,
                            started=time.monotonic(),
                        )
                    )
                    bytes_in += protected_bytes_in
                except (
                    asyncio.TimeoutError,
                    ConnectionError,
                    DaemonError,
                    OSError,
                    SkillFsPeerAuthError,
                ) as exc:
                    raise _SilentClose from exc

                request = parse_request_line(
                    request_line,
                    max_request_bytes=self.max_request_bytes,
                )
                parsed_request_method = request.method
                if request.method != METHOD_SKILLFS_NOTIFY_CHANGE:
                    raise BadRequestError(
                        "authenticated daemon connection only accepts "
                        f"{METHOD_SKILLFS_NOTIFY_CHANGE}"
                    )
            else:
                request = parse_request_line(
                    initial_frame,
                    max_request_bytes=self.max_request_bytes,
                )
                parsed_request_method = request.method
                if (
                    self._notify_auth_secret is not None
                    and request.method == METHOD_SKILLFS_NOTIFY_CHANGE
                ):
                    raise _SilentClose

            prepared_request = self.gateway.prepare(request)
            response = await self.gateway.execute(prepared_request)
        except _SilentClose:
            silent_close = True
        except asyncio.TimeoutError:
            response = error_response(
                fallback_request_id,
                DaemonTimeoutError(self.request_read_timeout_ms),
            )
        except DaemonError as exc:
            response = error_response(
                _request_id_for_response(prepared_request, fallback_request_id),
                exc,
            )
        except Exception:
            request_id = _request_id_for_response(prepared_request, fallback_request_id)
            LOGGER.exception(
                "unexpected daemon request failure",
                extra={
                    "diagnostic_event": "daemon_request_internal_error",
                    "diagnostic_request_id": request_id,
                    "data": {
                        "request_id": request_id,
                        "method": (
                            None
                            if prepared_request is None
                            else prepared_request.request.method
                        ),
                    },
                },
            )
            response = error_response(request_id, InternalDaemonError())
        finally:
            if silent_close:
                await self._close_writer(writer)
            else:
                if response is None:
                    response = error_response(
                        _request_id_for_response(
                            prepared_request,
                            fallback_request_id,
                        ),
                        ShutdownError(),
                    )
                with contextlib.suppress(
                    ConnectionError,
                    BrokenPipeError,
                    OSError,
                    asyncio.CancelledError,
                ):
                    response_bytes_out, response = await self._write_response(
                        writer,
                        response,
                        authenticated_session=authenticated_session,
                    )
                    bytes_out += response_bytes_out
                if prepared_request is not None:
                    self.gateway.complete(
                        prepared=prepared_request,
                        response=response,
                        started=started,
                        bytes_in=bytes_in,
                        bytes_out=bytes_out,
                    )
                elif not response.ok:
                    _log_request_completion(
                        request_id=fallback_request_id,
                        method=parsed_request_method,
                        response=response,
                        started=started,
                        bytes_in=bytes_in,
                        bytes_out=bytes_out,
                    )

    async def _authenticate_notify_client(
        self,
        frame_reader: _ConnectionFrameReader,
        writer: asyncio.StreamWriter,
        secret: SharedSecret,
        deadline: float,
    ) -> tuple[AuthenticatedSession, int, int]:
        nonce, challenge_frame = build_auth_challenge_frame()
        await self._write_auth_frame(writer, challenge_frame, deadline)

        proof_frame = await frame_reader.read_frame(
            _AUTH_WIRE_FRAME_LIMIT,
            deadline=deadline,
            allow_eof_terminated=False,
        )
        verify_auth_proof_frame(
            proof_frame,
            expected_kind="auth.proof",
            secret=secret,
            domain=NOTIFY_CLIENT_DOMAIN,
            nonce=nonce,
        )

        accepted_frame = build_auth_proof_frame(
            "auth.ok",
            secret,
            NOTIFY_SERVER_DOMAIN,
            nonce,
        )
        await self._write_auth_frame(writer, accepted_frame, deadline)
        return (
            AuthenticatedSession(
                secret,
                nonce,
                NOTIFY_CLIENT_DOMAIN,
                NOTIFY_SERVER_DOMAIN,
            ),
            len(proof_frame),
            len(challenge_frame) + len(accepted_frame),
        )

    async def _read_authenticated_request(
        self,
        frame_reader: _ConnectionFrameReader,
        session: AuthenticatedSession,
        *,
        started: float,
    ) -> tuple[bytes, int]:
        deadline = started + (self.request_read_timeout_ms / 1000)
        payload_frame = await frame_reader.read_frame(
            self.max_request_bytes,
            deadline=deadline,
            allow_eof_terminated=False,
        )
        auth_frame = await frame_reader.read_frame(
            _AUTH_WIRE_FRAME_LIMIT,
            deadline=deadline,
            allow_eof_terminated=False,
        )
        payload = payload_frame[:-1]
        session.verify_payload(payload, auth_frame, "client")
        return payload, len(payload_frame) + len(auth_frame)

    async def _write_auth_frame(
        self,
        writer: asyncio.StreamWriter,
        frame: bytes,
        deadline: float,
    ) -> None:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise asyncio.TimeoutError
        writer.write(frame)
        await asyncio.wait_for(writer.drain(), timeout=remaining)

    async def _write_response(
        self,
        writer: asyncio.StreamWriter,
        response: DaemonResponse,
        authenticated_session: AuthenticatedSession | None = None,
    ) -> tuple[int, DaemonResponse]:
        try:
            raw_response = serialize_response(response)
        except Exception:
            LOGGER.exception(
                "failed to serialize daemon response",
                extra={
                    "diagnostic_event": "daemon_response_serialize_failed",
                    "diagnostic_request_id": response.request_id,
                    "data": {"request_id": response.request_id},
                },
            )
            response = error_response(response.request_id, InternalDaemonError())
            raw_response = serialize_response(response)
        if len(raw_response) > self.max_response_bytes:
            response = error_response(
                response.request_id, ResponseTooLargeError(self.max_response_bytes)
            )
            raw_response = serialize_response(response)
            if len(raw_response) > self.max_response_bytes:
                raw_response = b""

        if raw_response and authenticated_session is not None:
            raw_response = authenticated_session.protect_payload(
                raw_response[:-1],
                "server",
            )

        bytes_out = 0
        try:
            if raw_response:
                writer.write(raw_response)
                bytes_out = len(raw_response)
                with contextlib.suppress(ConnectionError, BrokenPipeError):
                    await writer.drain()
            return bytes_out, response
        finally:
            await self._close_writer(writer)

    async def _close_writer(self, writer: asyncio.StreamWriter) -> None:
        writer.close()
        with contextlib.suppress(
            ConnectionError,
            BrokenPipeError,
            OSError,
            asyncio.CancelledError,
        ):
            await writer.wait_closed()

    async def _write_immediate_error(
        self,
        writer: asyncio.StreamWriter,
        error: DaemonError,
    ) -> None:
        request_id = generate_request_id()
        started = time.monotonic()
        response = error_response(request_id, error)
        bytes_out, response = await self._write_response(writer, response)
        _log_request_completion(
            request_id=request_id,
            method=None,
            response=response,
            started=started,
            bytes_in=0,
            bytes_out=bytes_out,
            trace_context=None,
        )

    async def _drain_client_tasks(self) -> None:
        current_task = asyncio.current_task()
        pending_tasks = {
            task
            for task in self._client_tasks
            if not task.done() and task is not current_task
        }
        if not pending_tasks:
            return

        _done, still_pending = await asyncio.wait(
            pending_tasks,
            timeout=self._drain_timeout_seconds,
        )
        for task in still_pending:
            task.cancel()
        if still_pending:
            with contextlib.suppress(asyncio.CancelledError):
                await asyncio.gather(*still_pending)

    def _set_daemon_umask(self) -> None:
        if self._previous_umask is None:
            self._previous_umask = os.umask(0o077)

    def _restore_umask(self) -> None:
        if self._previous_umask is not None:
            os.umask(self._previous_umask)
            self._previous_umask = None


def prepare_socket_path(socket_path: Path) -> SingleInstanceLock:
    """Prepare runtime directory, remove stale sockets, and acquire lock."""
    ensure_runtime_directory(socket_path)

    lock = SingleInstanceLock(lock_path_for_socket(socket_path))
    lock.acquire()

    try:
        if _path_exists(socket_path) and daemon_health_reachable(socket_path):
            raise DaemonAlreadyRunningError()

        if _path_exists(socket_path):
            _unlink_stale_socket(socket_path)
    except Exception:
        lock.release()
        raise

    return lock


def configure_logging() -> None:
    """Initialize daemon diagnostic logging."""
    # The daemon owns process-level logging. Keep root permissive so records
    # from agent-sec and third-party libraries can reach the JSONL handler;
    # AGENT_SEC_DAEMON_LOG_LEVEL is enforced by that handler, not by root.
    logging.getLogger().setLevel(logging.DEBUG)
    LOGGER.setLevel(logging.NOTSET)
    LOGGER.propagate = True
    setup_daemon_logging()


def _request_id_for_response(
    prepared_request: PreparedDaemonRequest | None,
    fallback_request_id: str,
) -> str:
    if prepared_request is None:
        return fallback_request_id
    return prepared_request.request.request_id


async def run_daemon(
    socket_path: str | Path | None = None,
    max_request_bytes: int = DEFAULT_MAX_REQUEST_BYTES,
    max_response_bytes: int = DEFAULT_MAX_RESPONSE_BYTES,
    max_connections: int = DEFAULT_MAX_CONNECTIONS,
    request_read_timeout_ms: int = DEFAULT_REQUEST_READ_TIMEOUT_MS,
) -> None:
    """Run the daemon until SIGTERM/SIGINT or task cancellation."""
    configure_logging()
    daemon = DaemonServer(
        socket_path=socket_path,
        max_request_bytes=max_request_bytes,
        max_response_bytes=max_response_bytes,
        max_connections=max_connections,
        request_read_timeout_ms=request_read_timeout_ms,
    )
    stop_event = asyncio.Event()
    _install_signal_handlers(stop_event)
    await daemon.start()
    log_daemon_event(
        event="daemon_started",
        message="daemon started",
        data=_daemon_lifecycle_data(daemon),
    )
    try:
        await stop_event.wait()
    finally:
        await daemon.stop()
        log_daemon_event(
            event="daemon_stopped",
            message="daemon stopped",
            data=_daemon_lifecycle_data(daemon),
        )


def main(argv: Sequence[str] | None = None) -> None:
    """CLI entry point for manual daemon execution."""
    parser = _build_arg_parser()
    args = parser.parse_args(argv)
    command = args.command or "serve"
    if command != "serve":
        parser.error(f"unknown command: {command}")

    asyncio.run(
        run_daemon(
            socket_path=args.socket_path,
            max_request_bytes=args.max_request_bytes,
            max_response_bytes=args.max_response_bytes,
            max_connections=args.max_connections,
            request_read_timeout_ms=args.request_read_timeout_ms,
        )
    )


def _build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="agent-sec-daemon")
    subparsers = parser.add_subparsers(dest="command")
    serve_parser = subparsers.add_parser("serve")
    serve_parser.add_argument(
        "--socket", dest="socket_path", default=None, help=argparse.SUPPRESS
    )
    serve_parser.add_argument(
        "--max-request-bytes", type=int, default=DEFAULT_MAX_REQUEST_BYTES
    )
    serve_parser.add_argument(
        "--max-response-bytes",
        type=int,
        default=DEFAULT_MAX_RESPONSE_BYTES,
        help=argparse.SUPPRESS,
    )
    serve_parser.add_argument(
        "--max-connections", type=int, default=DEFAULT_MAX_CONNECTIONS
    )
    serve_parser.add_argument(
        "--request-read-timeout-ms",
        type=int,
        default=DEFAULT_REQUEST_READ_TIMEOUT_MS,
        help=argparse.SUPPRESS,
    )
    parser.set_defaults(socket_path=None)
    parser.set_defaults(max_request_bytes=DEFAULT_MAX_REQUEST_BYTES)
    parser.set_defaults(max_response_bytes=DEFAULT_MAX_RESPONSE_BYTES)
    parser.set_defaults(max_connections=DEFAULT_MAX_CONNECTIONS)
    parser.set_defaults(request_read_timeout_ms=DEFAULT_REQUEST_READ_TIMEOUT_MS)
    return parser


def _install_signal_handlers(stop_event: asyncio.Event) -> None:
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        with contextlib.suppress(NotImplementedError):
            loop.add_signal_handler(sig, stop_event.set)
    if hasattr(signal, "SIGHUP"):
        with contextlib.suppress(NotImplementedError):
            loop.add_signal_handler(signal.SIGHUP, _log_sighup_noop)


def _daemon_lifecycle_data(daemon: DaemonServer) -> dict[str, Any]:
    return {
        "socket_path": str(daemon.socket_path),
        "max_request_bytes": daemon.max_request_bytes,
        "max_response_bytes": daemon.max_response_bytes,
        "max_connections": daemon.max_connections,
        "request_read_timeout_ms": daemon.request_read_timeout_ms,
    }


def _log_sighup_noop() -> None:
    log_daemon_event(
        event="daemon_sighup_noop",
        message="daemon SIGHUP ignored",
    )


def _path_exists(path: Path) -> bool:
    try:
        path.lstat()
    except FileNotFoundError:
        return False
    return True


def _unlink_stale_socket(socket_path: Path) -> None:
    try:
        socket_stat = socket_path.lstat()
    except FileNotFoundError:
        return

    if not stat.S_ISSOCK(socket_stat.st_mode):
        raise DaemonRuntimePathError(
            f"socket path exists and is not a socket: {socket_path}"
        )
    socket_path.unlink()


def _socket_identity(socket_path: Path) -> SocketIdentity | None:
    try:
        socket_stat = socket_path.lstat()
    except FileNotFoundError:
        return None

    if not stat.S_ISSOCK(socket_stat.st_mode):
        return None
    return (socket_stat.st_dev, socket_stat.st_ino)


def _unlink_socket_if_owned(
    socket_path: Path,
    expected_identity: SocketIdentity | None,
) -> None:
    try:
        socket_stat = socket_path.lstat()
    except FileNotFoundError:
        return

    if not stat.S_ISSOCK(socket_stat.st_mode):
        return
    if (
        expected_identity is not None
        and (
            socket_stat.st_dev,
            socket_stat.st_ino,
        )
        != expected_identity
    ):
        return

    socket_path.unlink()


if __name__ == "__main__":
    main()
