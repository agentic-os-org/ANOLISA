"""Keep Profile stream execution and cleanup bound to the originating request."""

from __future__ import annotations

import asyncio
import threading
from collections.abc import AsyncGenerator, Callable, Generator
from concurrent.futures import ThreadPoolExecutor
from contextvars import Context, copy_context
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any
from weakref import finalize

from aw_sdk.sources import SourceSession
from aw_sdk.transport import AwError

if TYPE_CHECKING:
    from aw_sdk.profile_source import ProfileSource


@dataclass
class _Request:
    session: SourceSession
    sync_streams: set[_SyncStream] = field(default_factory=set)
    cleanup_tasks: set[asyncio.Future] = field(default_factory=set)
    cleanup_error: BaseException | None = None
    calls: int = 0
    ended: bool = False
    drained: asyncio.Event = field(default_factory=asyncio.Event)

    def require_live(self) -> None:
        if self.ended and self.session._native_owner() is not self:
            raise AwError("source_request_session_required")


class _StreamContext:
    """Run every step and finalizer in the context of the first consumer."""

    def __init__(self, source: ProfileSource, request: _Request) -> None:
        self.source, self.request = source, request
        self.context: Context | None = None

    def resume(self) -> Context:
        self.source._check_request(self.request)
        if self.context is None:
            self.context = copy_context()
        return self.context


class _SyncStream(_StreamContext):
    """The request owns a started iterator through its last step or close."""

    def __init__(self, stream: Generator, source: ProfileSource, request: _Request):
        super().__init__(source, request)
        self.stream = stream
        self.lock = threading.Lock()

    def advance(self, method: Callable, *args: Any) -> Any:
        if not self.lock.acquire(blocking=False):
            raise ValueError("generator already executing")
        try:
            with self.source._lock:
                context = self.resume()
                if self.stream.gi_frame is not None:
                    if self.source._sync_closer is None:
                        # Reserve cleanup capacity before native work starts. The
                        # application can shut down its executor at any later point.
                        closer = ThreadPoolExecutor(
                            max_workers=1, thread_name_prefix="aw-stream-close"
                        )
                        try:
                            closer.submit(lambda: None)
                        except RuntimeError:
                            closer.shutdown(wait=False, cancel_futures=True)
                            self.context = None
                            raise
                        self.source._sync_closer = closer
                    self.request.sync_streams.add(self)
            return context.run(method, *args)
        finally:
            if self.stream.gi_frame is None:
                self.source._settle_stream(self.request, self)
                self.context = None
            self.lock.release()
            if self.request.ended and self.stream.gi_frame is not None:
                self.source._loop.call_soon_threadsafe(self.schedule_close)

    def close(self) -> None:
        if not self.lock.acquire(blocking=False):
            raise ValueError("generator already executing")
        self._close_locked(abandoned=False)

    def _close_locked(self, abandoned: bool) -> None:
        error = None
        try:
            if self.context is None:
                self.stream.close()
            else:
                self.context.run(self.stream.close)
        except BaseException as failure:
            if not abandoned:
                raise
            error = failure
        finally:
            self.source._settle_stream(self.request, self, error)
            self.context = None
            self.lock.release()

    def _close_abandoned(self) -> None:
        with self.source._lock:
            if self not in self.request.sync_streams or not self.lock.acquire(
                blocking=False
            ):
                return
        self._close_locked(abandoned=True)

    def schedule_close(self) -> None:
        # An in-flight step schedules close again on return. Claim the stream in
        # the worker so a failed submission never strands the stream lock.
        with self.source._lock:
            if self not in self.request.sync_streams or self.lock.locked():
                return
        try:
            self.source._loop.run_in_executor(None, self._close_abandoned)
        except RuntimeError:
            self.source._sync_closer.submit(self._close_abandoned)


class _RequestStream(Generator):
    def __init__(self, stream: Generator, source: ProfileSource, request: _Request):
        self.owner = _SyncStream(stream, source, request)

    def send(self, value: Any) -> Any:
        return self.owner.advance(self.owner.stream.send, value)

    def throw(self, *exc: Any) -> Any:
        return self.owner.advance(self.owner.stream.throw, *exc)

    def close(self) -> None:
        self.owner.close()

    def __del__(self) -> None:
        if self.owner.context is not None and self.owner.stream.gi_frame is not None:
            loop = self.owner.source._loop
            if not loop.is_closed():
                loop.call_soon_threadsafe(self.owner.schedule_close)


class _AsyncStreamRunner:
    """Serialize native operations in one Task, including a foreign caller's close."""

    def __init__(self, stream: AsyncGenerator, context: Context) -> None:
        self.stream = stream
        self.loop = asyncio.get_running_loop()
        self.commands: asyncio.Queue = asyncio.Queue()
        self.interrupted: asyncio.Future | None = None
        self.claimed = asyncio.Event()
        self.task = self.loop.create_task(self._run(), context=context)
        self.busy = self.closing = False

    async def _run(self) -> None:
        while True:
            try:
                command = await self.commands.get()
            except asyncio.CancelledError as error:
                # A deadline can expire between reads. Throw at the yield and
                # deliver its outcome to the pending reader or the next one.
                if self.commands.empty():
                    reply = self.interrupted = self.loop.create_future()
                    self.claimed.clear()
                else:
                    command = self.commands.get_nowait()
                    if command is None:
                        break
                    _, _, reply = command
                method, args = self.stream.athrow, (error,)
            else:
                if command is None:
                    break
                method, args, reply = command
            try:
                value = await method(*args)
            except BaseException as error:  # noqa: BLE001
                # An unattended timeout has no reader yet to receive its error.
                reply.set_result((None, error))
            else:
                reply.set_result((value, None))
            if self.stream.ag_frame is None:
                return
            if self.interrupted is reply:
                # A generator can catch the idle cancellation and yield. Apply
                # backpressure until that single item is claimed or closed.
                try:
                    await self.claimed.wait()
                except asyncio.CancelledError:
                    break
        await self.stream.aclose()

    async def read(self, method: Callable, args: tuple[Any, ...]) -> Any:
        if self.busy or (self.closing and not self.task.done()):
            raise RuntimeError("asynchronous generator is already running")
        if self.interrupted is not None:
            reply, self.interrupted = self.interrupted, None
            self.claimed.set()
        elif self.task.done():
            return await method(*args)
        else:
            reply = self.loop.create_future()
            self.commands.put_nowait((method, args, reply))
        self.busy = True
        try:
            cancelled = False
            while True:
                try:
                    await asyncio.wait(
                        (reply, self.task), return_when=asyncio.FIRST_COMPLETED
                    )
                    break
                except asyncio.CancelledError as error:
                    if reply.done():
                        raise
                    if not cancelled:
                        self.task.cancel(*error.args)
                        cancelled = True
            if not reply.done():
                # The runner finalized while this operation was queued.
                self.task.result()
                return await method(*args)
            value, error = reply.result()
            if error is not None:
                raise error
            return value
        finally:
            self.busy = False

    def _start_close(self) -> None:
        if not self.closing:
            self.closing = True
            self.claimed.set()
            self.commands.put_nowait(None)

    def abandon(self) -> None:
        if not self.loop.is_closed() and not self.task.done():
            self.loop.call_soon_threadsafe(self._start_close)

    async def close(self) -> None:
        if asyncio.get_running_loop() is not self.loop:
            raise AwError("source_activation_loop_required")
        if self.busy and not self.task.done():
            raise RuntimeError("aclose(): asynchronous generator is already running")
        self._start_close()
        await asyncio.shield(self.task)
        self.interrupted = None


class _RequestAsyncStream(AsyncGenerator):
    def __init__(
        self, stream: AsyncGenerator, source: ProfileSource, request: _Request
    ) -> None:
        self.stream = stream
        self.owner = _StreamContext(source, request)
        self.runner: _AsyncStreamRunner | None = None

    def _reader(self) -> _AsyncStreamRunner:
        self.owner.source._check_stream_loop()
        context = self.owner.resume()
        if self.runner is None:
            self.runner = _AsyncStreamRunner(self.stream, context)
            finalize(self, self.runner.abandon)
        return self.runner

    async def asend(self, value: Any) -> Any:
        return await self._reader().read(self.stream.asend, (value,))

    async def athrow(self, *exc: Any) -> Any:
        return await self._reader().read(self.stream.athrow, exc)

    async def aclose(self) -> None:
        if self.runner is None:
            await self.stream.aclose()
        else:
            await self.runner.close()
