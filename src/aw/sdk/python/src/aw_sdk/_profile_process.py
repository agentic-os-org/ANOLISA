"""Bridge preloaded request Scopes to the SDK loop without moving native calls."""

from __future__ import annotations

import asyncio
from collections.abc import AsyncGenerator, AsyncIterator, Callable, Generator, Iterator
from concurrent.futures import Future
from contextlib import asynccontextmanager, contextmanager
from typing import Any

from aw_sdk._profile_stream import _Request
from aw_sdk.profile_source import ProfileSource
from aw_sdk.sources import SourceSession
from aw_sdk.transport import AwError


class _Scope:
    """Keep both sides of a ProfileSource request in one owning SDK Task."""

    def __init__(self, source: ProcessProfile, session: SourceSession | None = None):
        self.source = source
        self.opened: Future[_Request | None] = Future()
        self.stop = asyncio.Event()
        self.finished = asyncio.run_coroutine_threadsafe(
            self._run(session), source._loop
        )

    async def _run(self, session: SourceSession | None) -> None:
        try:
            async with ProfileSource.request(self.source, session):
                self.opened.set_result(self.source._request_context.get(None))
                await self.stop.wait()
        except BaseException as error:
            if not self.opened.done():
                self.opened.set_exception(error)
            raise

    def close(self) -> Future[None]:
        self.source._loop.call_soon_threadsafe(self.stop.set)
        return self.finished

    @contextmanager
    def use(self, request: _Request | None) -> Iterator[None]:
        token = self.source._request_context.set(request)
        try:
            yield
        finally:
            self.source._request_context.reset(token)


class ProcessProfile(ProfileSource):
    """Preloaded SDK source with a default Scope and optional explicit requests."""

    default_request: _Request | None = None
    cooperative = False
    stopping = False

    def _current_request(self) -> _Request | None:
        return self._request_context.get(self.default_request)

    def _check_stream_loop(self) -> None:
        # The application's reader owns native stream iteration. Only SDK RPC
        # and its admission run on the private activation loop.
        asyncio.get_running_loop()

    async def _async(self, hook: Any) -> Any:
        if asyncio.get_running_loop() is self._loop:
            return await hook
        return await asyncio.wrap_future(
            asyncio.run_coroutine_threadsafe(hook, self._loop)
        )

    @asynccontextmanager
    async def request(
        self, session: SourceSession | None = None
    ) -> AsyncIterator[SourceSession | None]:
        """Admit an explicit request while retaining the caller's application loop."""
        if self.stopping:
            raise AwError("source_unavailable")
        if asyncio.get_running_loop() is self._loop:
            async with super().request(session) as admitted:
                yield admitted
            return
        scope = _Scope(self, session)
        try:
            request = await asyncio.shield(asyncio.wrap_future(scope.opened))
            with scope.use(request):
                yield request.session if request is not None else session
        finally:
            await asyncio.shield(asyncio.wrap_future(scope.close()))

    def _wrap_process(self, call: Callable, shape: str) -> Callable:
        def implicit() -> bool:
            if self.stopping:
                raise AwError("source_unavailable")
            return self.cooperative and self._current_request() is None

        if shape == "async":

            def invoke(*args: Any, **kwargs: Any) -> Any:
                if not implicit():
                    return call(*args, **kwargs)

                async def scoped() -> Any:
                    async with self.request():
                        return await call(*args, **kwargs)

                return scoped()

            return invoke

        if shape == "async_iter":

            def async_stream(*args: Any, **kwargs: Any) -> AsyncGenerator:
                if not implicit():
                    return call(*args, **kwargs)
                return _AsyncScopeStream(self, call, args, kwargs)

            return async_stream

        if shape == "sync_iter":

            def sync_stream(*args: Any, **kwargs: Any) -> Generator:
                if not implicit():
                    return call(*args, **kwargs)
                return _SyncScopeStream(self, call, args, kwargs)

            return sync_stream

        def sync(*args: Any, **kwargs: Any) -> Any:
            if not implicit():
                return call(*args, **kwargs)
            scope = _Scope(self)
            try:
                with scope.use(scope.opened.result()):
                    return call(*args, **kwargs)
            finally:
                scope.close().result()

        return sync


class _SyncScopeStream(Generator):
    """Keep an implicit Scope through iteration without leaking its ContextVar."""

    def __init__(
        self, source: ProcessProfile, call: Callable, args: tuple, kwargs: dict
    ):
        self.source, self.call, self.args, self.kwargs = source, call, args, kwargs
        self.scope = self.stream = None

    def _step(self, operation: str, *args: Any) -> Any:
        if self.scope is None:
            if self.source.stopping:
                raise AwError("source_unavailable")
            self.scope = _Scope(self.source)
        try:
            with self.scope.use(self.scope.opened.result()):
                if self.stream is None:
                    self.stream = self.call(*self.args, **self.kwargs)
                return getattr(self.stream, operation)(*args)
        except BaseException:
            self.close()
            raise

    def send(self, value: Any) -> Any:
        return self._step("send", value)

    def throw(self, *exc: Any) -> Any:
        return self._step("throw", *exc)

    def close(self) -> None:
        if self.scope is not None:
            try:
                if self.stream is not None:
                    self.stream.close()
            finally:
                self.scope.close().result()

    def __del__(self) -> None:
        if self.scope is not None and self.source._loop.is_running():
            self.scope.close()


class _AsyncScopeStream(AsyncGenerator):
    """Own the Scope for all reads, including send, throw and early close."""

    def __init__(
        self, source: ProcessProfile, call: Callable, args: tuple, kwargs: dict
    ):
        self.source, self.call, self.args, self.kwargs = source, call, args, kwargs
        self.scope = self.stream = None

    async def _step(self, operation: str, *args: Any) -> Any:
        if self.scope is None:
            if self.source.stopping:
                raise AwError("source_unavailable")
            self.scope = _Scope(self.source)
        try:
            request = await asyncio.shield(asyncio.wrap_future(self.scope.opened))
            with self.scope.use(request):
                if self.stream is None:
                    self.stream = self.call(*self.args, **self.kwargs)
                return await getattr(self.stream, operation)(*args)
        except BaseException:
            await self.aclose()
            raise

    async def asend(self, value: Any) -> Any:
        return await self._step("asend", value)

    async def athrow(self, *exc: Any) -> Any:
        return await self._step("athrow", *exc)

    async def aclose(self) -> None:
        if self.scope is not None:
            try:
                if self.stream is not None:
                    await self.stream.aclose()
            finally:
                await asyncio.shield(asyncio.wrap_future(self.scope.close()))

    def __del__(self) -> None:
        if self.scope is not None and self.source._loop.is_running():
            self.scope.close()
