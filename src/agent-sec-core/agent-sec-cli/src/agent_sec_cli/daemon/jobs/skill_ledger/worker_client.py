"""Lifecycle and NDJSON transport for the Skill Ledger worker process."""

import asyncio
import contextlib
import sys
from typing import Any

from agent_sec_cli.daemon.jobs.skill_ledger.protocol import (
    MAX_WORKER_FRAME_BYTES,
    SkillFsChange,
    WorkerProtocolError,
    new_worker_request,
    parse_worker_response,
    serialize_worker_request,
)

_WORKER_MODULE = "agent_sec_cli.daemon.jobs.skill_ledger.worker"
DEFAULT_REQUEST_TIMEOUT_SECONDS = 300.0
_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS = 2.0
_TERMINATE_TIMEOUT_SECONDS = 5.0
_MAX_STDERR_BYTES = 64 * 1024


class SkillLedgerWorkerError(RuntimeError):
    """Base class for Skill Ledger worker failures."""


class SkillLedgerWorkerTransportError(SkillLedgerWorkerError):
    """Raised when the worker process or protocol transport fails."""


class SkillLedgerWorkerExecutionError(SkillLedgerWorkerError):
    """Raised when Skill Ledger processing fails inside a healthy worker."""

    def __init__(self, error_type: str, message: str) -> None:
        super().__init__(message)
        self.error_type = error_type


class SkillLedgerWorkerClient:
    """Own one lazily started, serial Skill Ledger worker process."""

    def __init__(
        self,
        request_timeout_seconds: float = DEFAULT_REQUEST_TIMEOUT_SECONDS,
    ) -> None:
        if request_timeout_seconds <= 0:
            raise ValueError("request_timeout_seconds must be positive")
        self._request_timeout_seconds = request_timeout_seconds
        self._lock = asyncio.Lock()
        self._process: asyncio.subprocess.Process | None = None
        self._stderr_task: asyncio.Task[None] | None = None
        self._stderr = bytearray()
        self._stopping = False
        self._last_error: str | None = None

    @property
    def pid(self) -> int | None:
        """Return the active worker PID, if one exists."""
        if self._process is None or self._process.returncode is not None:
            return None
        return self._process.pid

    @property
    def last_error(self) -> str | None:
        """Return the last worker transport error."""
        return self._last_error

    async def process_change(self, change: SkillFsChange) -> dict[str, Any]:
        """Process one change, restarting once after transport failure."""
        async with self._lock:
            for attempt in range(2):
                try:
                    try:
                        # Preserve external cancellation when the response completes.
                        async with asyncio.timeout(self._request_timeout_seconds):
                            result = await self._process_once(change)
                    except asyncio.TimeoutError as exc:
                        raise SkillLedgerWorkerTransportError(
                            "worker request timed out after "
                            f"{self._request_timeout_seconds:g}s"
                        ) from exc
                except asyncio.CancelledError:
                    await self._cancel_worker()
                    raise
                except SkillLedgerWorkerTransportError as exc:
                    self._last_error = str(exc)
                    await self._terminate_worker()
                    if attempt == 1:
                        raise
                    continue
                self._last_error = None
                return result
        raise AssertionError("worker retry loop must return or raise")

    async def stop(self) -> None:
        """Close stdin, then terminate and kill the worker if needed."""
        async with self._lock:
            self._stopping = True
            try:
                await self._shutdown_worker()
            finally:
                self._stopping = False

    async def _process_once(self, change: SkillFsChange) -> dict[str, Any]:
        process = await self._ensure_worker()
        if process.stdin is None or process.stdout is None:
            raise SkillLedgerWorkerTransportError("worker stdio is unavailable")

        request = new_worker_request(change)
        try:
            process.stdin.write(serialize_worker_request(request))
            await process.stdin.drain()
            line = await process.stdout.readline()
        except (BrokenPipeError, ConnectionError, OSError, ValueError) as exc:
            raise SkillLedgerWorkerTransportError(
                self._transport_message(f"worker communication failed: {exc}")
            ) from exc

        if not line:
            returncode = process.returncode
            exit_status = (
                f"exit code {returncode}"
                if returncode is not None
                else "closing stdout before exit"
            )
            raise SkillLedgerWorkerTransportError(
                self._transport_message(f"worker closed stdout with {exit_status}")
            )
        if len(line) > MAX_WORKER_FRAME_BYTES:
            raise SkillLedgerWorkerTransportError(
                f"worker response exceeds {MAX_WORKER_FRAME_BYTES} bytes"
            )

        try:
            response = parse_worker_response(line)
        except WorkerProtocolError as exc:
            raise SkillLedgerWorkerTransportError(
                self._transport_message(f"invalid worker response: {exc}")
            ) from exc
        if response.request_id != request.request_id:
            raise SkillLedgerWorkerTransportError(
                "worker response requestId does not match the request"
            )
        if not response.ok:
            if response.error is None:
                raise SkillLedgerWorkerTransportError(
                    "worker error response is missing error details"
                )
            raise SkillLedgerWorkerExecutionError(
                response.error.error_type,
                response.error.message,
            )
        if response.result is None:
            raise SkillLedgerWorkerTransportError("worker response is missing a result")
        return response.result

    async def _ensure_worker(self) -> asyncio.subprocess.Process:
        if self._stopping:
            raise SkillLedgerWorkerTransportError("worker is stopping")
        if self._process is not None and self._process.returncode is None:
            return self._process
        if self._process is not None:
            await self._dispose_exited_worker()

        self._stderr.clear()
        try:
            process = await asyncio.create_subprocess_exec(
                sys.executable,
                "-m",
                _WORKER_MODULE,
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                limit=MAX_WORKER_FRAME_BYTES + 1,
            )
        except OSError as exc:
            raise SkillLedgerWorkerTransportError(
                f"failed to start Skill Ledger worker: {exc}"
            ) from exc
        self._process = process
        if process.stderr is not None:
            self._stderr_task = asyncio.create_task(self._drain_stderr(process.stderr))
        return process

    async def _drain_stderr(self, reader: asyncio.StreamReader) -> None:
        while True:
            chunk = await reader.read(4096)
            if not chunk:
                return
            self._stderr.extend(chunk)
            if len(self._stderr) > _MAX_STDERR_BYTES:
                del self._stderr[: len(self._stderr) - _MAX_STDERR_BYTES]

    def _transport_message(self, message: str) -> str:
        stderr = self._stderr.decode("utf-8", errors="replace").strip()
        return f"{message}: {stderr}" if stderr else message

    async def _cancel_worker(self) -> None:
        cleanup_task = asyncio.create_task(self._terminate_worker())
        try:
            await asyncio.shield(cleanup_task)
        except asyncio.CancelledError:
            with contextlib.suppress(asyncio.CancelledError):
                await cleanup_task

    async def _shutdown_worker(self) -> None:
        process, stderr_task = self._detach_worker()
        if process is None:
            return
        await self._close_stdin(process)
        if process.returncode is None:
            try:
                await asyncio.wait_for(
                    process.wait(),
                    timeout=_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS,
                )
            except asyncio.TimeoutError:
                await self._terminate_process(process)
        await self._finish_stderr_task(stderr_task)

    async def _terminate_worker(self) -> None:
        process, stderr_task = self._detach_worker()
        if process is None:
            return
        await self._close_stdin(process)
        if process.returncode is None:
            await self._terminate_process(process)
        await self._finish_stderr_task(stderr_task)

    async def _terminate_process(self, process: asyncio.subprocess.Process) -> None:
        with contextlib.suppress(ProcessLookupError):
            process.terminate()
        try:
            await asyncio.wait_for(
                process.wait(),
                timeout=_TERMINATE_TIMEOUT_SECONDS,
            )
        except asyncio.TimeoutError:
            with contextlib.suppress(ProcessLookupError):
                process.kill()
            await process.wait()

    async def _dispose_exited_worker(self) -> None:
        process, stderr_task = self._detach_worker()
        if process is None:
            return
        await self._close_stdin(process)
        await process.wait()
        await self._finish_stderr_task(stderr_task)

    def _detach_worker(
        self,
    ) -> tuple[asyncio.subprocess.Process | None, asyncio.Task[None] | None]:
        process = self._process
        stderr_task = self._stderr_task
        self._process = None
        self._stderr_task = None
        return process, stderr_task

    async def _close_stdin(self, process: asyncio.subprocess.Process) -> None:
        if process.stdin is None:
            return
        process.stdin.close()
        with contextlib.suppress(BrokenPipeError, ConnectionError, OSError):
            await process.stdin.wait_closed()

    async def _finish_stderr_task(
        self,
        stderr_task: asyncio.Task[None] | None,
    ) -> None:
        if stderr_task is None:
            return
        caller = asyncio.current_task()
        cancellation_count = caller.cancelling() if caller is not None else 0
        if not stderr_task.done():
            stderr_task.cancel()
        try:
            await asyncio.shield(stderr_task)
        except asyncio.CancelledError:
            if caller is not None and caller.cancelling() > cancellation_count:
                raise
        except Exception:
            return
