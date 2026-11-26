"""On-demand Python instrumentation over the AW language Source protocol."""

from __future__ import annotations

import asyncio
import sys
import threading
from collections.abc import AsyncIterator, Coroutine, Iterator
from contextlib import asynccontextmanager, contextmanager
from contextvars import ContextVar
from copy import deepcopy
from dataclasses import dataclass, field
from importlib.metadata import PackageNotFoundError, version
from time import monotonic
from types import ModuleType
from typing import Any, TypeVar

from aw_sdk._profile_call import Invocation, SessionIdentity, build_wrapper
from aw_sdk._profile_eval import (
    CallFields,
    compile_hooks,
    parse_profile,
    version_matches,
)
from aw_sdk._profile_patch import (
    Patch,
    ProfileFinder,
    Seam,
    function_code_sha256,
)
from aw_sdk._profile_stream import _SyncStream
from aw_sdk._source_helpers import validate_identity
from aw_sdk._source_profile import SUPPORTED_EVENTS
from aw_sdk.client import Instance
from aw_sdk.sources import (
    SOURCE_PROTOCOL,
    LanguageSource,
    SourceSession,
    confirm_source_routes,
)
from aw_sdk.transport import AwError

__all__ = ["ProfileSource", "function_code_sha256"]

_Result = TypeVar("_Result")


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


class ProfileSource:
    """Install, verify and activate a Python instrumentation profile once.

    Call `probe`, `install` and `verify` before awaiting `activate`. Keep the
    activation loop running for AW hooks. Invoke synchronous targets off that
    loop (for example with `asyncio.to_thread`). Coroutine and iterator targets
    capture the current request when called and enter its Scope when
    execution starts. Run targets inside `async with profile.request()` and
    await `teardown` on the activation loop after serving requests.

    Use `async` for coroutine functions and `async_iter` for async generator
    functions or synchronous factories of async iterators. Use `sync_iter` for
    generators and iterator factories. An async stream runs in one Task on the
    activation loop, with the context of its first read through finalization.
    Tool inputs must map to argument fields so the native method retains its receiver.

    Mapped session IDs take priority over the context variable or callback,
    then the session's default identity. Hooks for session start, input,
    turn stop and tool admission precede native execution. Tool results and
    session end are reported upon successful completion. The SDK lifecycle
    uses a fixed reason for turn stop and session end.
    """

    def __init__(
        self,
        profile: bytes | str | dict[str, Any],
        *,
        availability: str = "required",
        session_id: str | None = None,
        session_id_context: SessionIdentity | None = None,
    ) -> None:
        document = parse_profile(profile)
        self._hooks = compile_hooks(document["targets"])
        if any(event not in SUPPORTED_EVENTS for event in self._hooks):
            raise AwError("semantic_event_unavailable")
        package = document["package"]
        try:
            installed = version(package["name"])
        except PackageNotFoundError as error:
            raise AwError("source_probe_package_missing") from error
        if not version_matches(installed, package["versions"]):
            raise AwError("source_probe_package_mismatch")
        if availability not in ("required", "native_fallback"):
            raise AwError("availability_required")
        self._availability = availability
        self._session_id = None if session_id is None else validate_identity(session_id)
        self._session_id_context = session_id_context
        self._targets: dict[str, list[dict[str, Any]]] = {}
        for target in document["targets"]:
            fields = target["fields"]
            if ("input" in fields and fields["input"]["codec"] == "text") or (
                "turn.stop" in target["hooks"] and "output" in fields
            ):
                raise AwError("source_probe_field_unsupported")
            self._targets.setdefault(target["module"], []).append(target)
            module = sys.modules.get(target["module"])
            if target["module"] in sys.modules:
                if not isinstance(module, ModuleType):
                    raise AwError("source_probe_target_unavailable")
                Seam.resolve(module, target)

        # The SDK wire carries block/report through event effects and has no
        # interactive ask route. Preserve the full intersection in `hooks`.
        self._capabilities: dict[str, Any] = {
            "adapter": "sdk",
            "version": SOURCE_PROTOCOL,
            "entrypoint": "application",
            "events": {
                event: [effect for effect in cap["effects"] if effect != "ask"]
                for event, cap in self._hooks.items()
            },
        }
        if "withhold_result" in self._hooks.get("tool.after", {}).get(
            "failure_actions", ()
        ):
            self._capabilities["failure_actions"] = {"tool.after": ["withhold_result"]}
        self._lock = threading.RLock()
        self._sync_closer = None
        self._state = "probed"
        self._activation_epoch = 0
        self._requests: set[asyncio.Task[BaseException | None]] = set()
        self._finder = ProfileFinder(
            set(self._targets), self._check_import, self._loaded, self._import_failed
        )
        self._patches: list[Patch] = []
        self._bound: dict[str, ModuleType] = {}
        self._loop: asyncio.AbstractEventLoop | None = None
        self._drain_task: asyncio.Task[BaseException | None] | None = None
        self._source: LanguageSource | None = None
        self._request_context: ContextVar[_Request] = ContextVar("aw_profile_request")
        self._timeout_ms = 5000
        self._owns_source = False
        self._poisoned = False
        self._cleanup_unconfirmed = False

    @classmethod
    def probe(
        cls,
        profile: bytes | str | dict[str, Any],
        *,
        availability: str = "required",
        session_id: str | None = None,
        session_id_context: SessionIdentity | None = None,
    ) -> ProfileSource:
        """Validate without importing the declared target modules or patching them."""
        return cls(
            profile,
            availability=availability,
            session_id=session_id,
            session_id_context=session_id_context,
        )

    @property
    def hooks(self) -> dict[str, dict[str, Any]]:
        """Conservative profile capabilities, including per-event authority."""
        return deepcopy(self._hooks)

    @property
    def capabilities(self) -> dict[str, Any]:
        """Profile capabilities projected onto the SDK registration protocol."""
        return deepcopy(self._capabilities)

    @property
    def activation_epoch(self) -> int:
        return self._activation_epoch

    @property
    def availability(self) -> str:
        return self._availability

    def _remove_finder(self) -> None:
        if self._finder in sys.meta_path:
            sys.meta_path.remove(self._finder)

    def _restore(self) -> Exception | None:
        failed = []
        failure = None
        for patch in reversed(self._patches):
            try:
                patch.restore()
            except Exception as error:  # noqa: BLE001
                # Retain failures and restore the remaining attributes.
                failed.append(patch)
                if failure is None:
                    failure = error
        self._patches = list(reversed(failed))
        self._bound.clear()
        return failure

    def _fail(self) -> None:
        self._state = "failed"
        self._poisoned = self.availability == "required"
        if self._poisoned:
            return  # Explicit teardown releases the installed guards.
        self._remove_finder()
        self._restore()

    def _patch(self, modules: list[tuple[str, ModuleType]]) -> None:
        seams = [
            Seam.resolve(module, target)
            for name, module in modules
            for target in self._targets[name]
        ]
        holders = {(id(p.seam.holder), p.seam.name) for p in self._patches}
        for seam in seams:
            key = id(seam.holder), seam.name
            if key in holders:
                raise AwError("source_probe_target_overlap")
            holders.add(key)
            patch = Patch.create(seam, build_wrapper(seam, self))
            self._patches.append(patch)
            patch.install()
        self._bound.update(modules)

    def _check_import(self) -> None:
        with self._lock:
            if self._state == "failed" and self._poisoned:
                raise AwError("source_unavailable")

    def _loaded(self, module: ModuleType) -> None:
        with self._lock:
            if self._state == "closed" or self._finder not in sys.meta_path:
                return
            if self._state == "failed":
                if self._poisoned:
                    raise AwError("source_unavailable")
                return
            name = module.__name__
            if self._bound.get(name) is not module:
                self._patch([(name, module)])

    def _import_failed(self, error: BaseException) -> bool:
        with self._lock:
            if self._state == "draining":
                return self.availability == "required"
            if self._state == "closed":
                return False
            # A rejected reload leaves the still-active module intact.
            if (
                isinstance(error, AwError)
                and error.code == "source_probe_target_reload_unsupported"
            ):
                return True
            if self._state != "failed":
                self._fail()
            return self._poisoned

    def install(self) -> None:
        """Wrap loaded targets and register the deferred target import hook."""
        with self._lock:
            if self._state != "probed":
                raise AwError("source_profile_state_invalid")
            sys.meta_path.insert(0, self._finder)
            try:
                loaded = []
                for name in self._targets:
                    module = sys.modules.get(name)
                    if name not in sys.modules:
                        continue
                    if not isinstance(module, ModuleType):
                        raise AwError("source_probe_target_unavailable")
                    if self._finder.loading(module):
                        continue
                    if getattr(
                        getattr(module, "__spec__", None), "_initializing", False
                    ):
                        raise AwError("source_probe_target_import_in_progress")
                    loaded.append((name, module))
                self._patch(loaded)
                self._state = "installed"
            except BaseException as error:
                self._fail()
                if not isinstance(error, Exception) or isinstance(error, AwError):
                    raise
                raise AwError("source_install_failed") from error

    def _check_patches(self) -> None:
        if self._finder not in sys.meta_path:
            raise AwError("source_verify_failed")
        for name in self._targets:
            module = sys.modules.get(name)
            if name in self._bound:
                if self._bound[name] is not module:
                    raise AwError("source_verify_failed")
            elif name in sys.modules and (
                not isinstance(module, ModuleType) or not self._finder.loading(module)
            ):
                raise AwError("source_verify_failed")
        if any(not patch.intact() for patch in self._patches):
            raise AwError("source_verify_failed")

    def verify(self) -> None:
        """Verify that each installed descriptor still belongs to the profile."""
        with self._lock:
            if self._state not in ("installed", "verified"):
                raise AwError(
                    "source_unavailable"
                    if self._poisoned
                    else "source_profile_state_invalid"
                )
            try:
                self._check_patches()
                self._state = "verified"
            except BaseException as error:
                self._fail()
                if not isinstance(error, Exception):
                    raise
                raise AwError("source_verify_failed") from error

    def _confirm_routes(self, routes: dict[str, Any]) -> bool:
        with self._lock:
            if self._state not in ("activating", "active"):
                return False
            try:
                self._check_patches()
            except Exception:
                self._fail()
                raise
        # The reserved observation step has no configured provider failure action.
        return confirm_source_routes(self._capabilities, routes) and all(
            (route.get("step") == "$aw.observe" and route.get("on_error") == "report")
            or route.get("on_error", "report")
            in self._hooks[route["event"]]["failure_actions"]
            for route in routes["hooks"]
        )

    def _check_source_capabilities(self, declared: dict[str, Any]) -> None:
        # A route describes mutation and failure handling, but may omit a
        # provider's block effect. Verify the registration ceiling as well.
        for key in ("events", "failure_actions"):
            expected = self._capabilities.get(key, {})
            offered = declared.get(key, {})
            if key == "events" and offered.keys() != expected.keys():
                raise AwError("source_capabilities_mismatch")
            if any(
                not set(effects) <= set(expected.get(event, ()))
                for event, effects in offered.items()
            ):
                raise AwError("source_capabilities_mismatch")

    async def activate(
        self,
        source: LanguageSource | None = None,
        *,
        endpoint: str | None = None,
        target: str | None = None,
        config: str | None = None,
        cooperative: bool = False,
        timeout_ms: int = 5000,
    ) -> LanguageSource:
        """Bind an SDK Instance, or listen for AW `connect --sdk-pid` in cooperative mode.

        Cooperative admission waits for a verified connection at request entry.
        Enter `async with profile.request()` around each business request, or
        pass its existing SDK session to `profile.request(session)`.
        """
        loop = asyncio.get_running_loop()
        with self._lock:
            if self._state == "installed" and self.availability == "required":
                self._fail()
                raise AwError("source_profile_state_invalid")
            if self._state != "verified":
                raise AwError(
                    "source_unavailable"
                    if self._poisoned
                    else "source_profile_state_invalid"
                )
            self._state = "activating"
            self._loop = loop
        try:
            if type(timeout_ms) is not int or not 100 <= timeout_ms <= 60000:
                raise AwError("invalid_attach_timeout")
            if source is None:
                if endpoint is None or target is None:
                    raise AwError("source_endpoint_and_target_required")
                source = LanguageSource(
                    Instance(
                        endpoint=endpoint,
                        target=target,
                        capabilities=self._capabilities,
                        availability=self.availability,
                        install=self._confirm_routes,
                        config=config,
                        timeout_ms=timeout_ms,
                    ),
                    owns_instance=True,
                )
                owns_source = True
            else:
                if cooperative or any(
                    option is not None for option in (endpoint, target, config)
                ):
                    raise AwError("source_activation_options_conflict")
                if source.instance.availability != self.availability:
                    raise AwError("source_availability_mismatch")
                self._check_source_capabilities(source.instance.capabilities)
                owns_source = False

            self._source, self._owns_source = source, owns_source
            self._timeout_ms = timeout_ms
            if cooperative:
                source.listen()
            else:
                connected = await source.connect()
                if not connected and self.availability == "required":
                    raise AwError("instance_not_ready")
                connection = source.instance.connection
                if connection is not None and not self._confirm_routes(
                    connection["routes"]
                ):
                    raise AwError("source_route_effect_unsupported")
            with self._lock:
                if self._state != "activating":
                    raise AwError("source_unavailable")
                self._check_patches()
                self._activation_epoch += 1
                self._state = "active"
        except BaseException:
            with self._lock:
                if self._state in ("activating", "failed"):
                    self._fail()
            if self._owns_source and self._drain_task is None:
                await source.close(timeout_ms=self._timeout_ms)
                self._owns_source = False
            raise
        return source

    @asynccontextmanager
    async def request(
        self, session: SourceSession | None = None
    ) -> AsyncIterator[SourceSession | None]:
        """Bind one child Scope to a business request on the activation loop.

        The caller owns a supplied session and closes it after its last use.
        Otherwise this context opens and closes a session. Await all calls and
        exhaust their iterators in the same request. Explicit close/aclose can
        release an iterator from another context. Request exit closes remaining
        synchronous iterators in the context of their first read. After a deferred
        target selects native fallback, this context yields the supplied session
        or None and the application continues natively.
        """
        if asyncio.get_running_loop() is not self._loop:
            raise AwError("source_activation_loop_required")
        if (
            self._state == "failed"
            and self.activation_epoch
            and self.availability == "native_fallback"
        ):
            yield session
            return
        with self._lock:
            if self._state != "active":
                raise AwError("source_unavailable")
            source = self._source
            owned = session is None
            if owned:
                session = source.session(
                    self._session_id, events=self._hooks, timeout_ms=self._timeout_ms
                )
                session.admission.install = self._install_request_routes
            elif session._source is not source or not self._hooks.keys() <= set(
                session.events
            ):
                raise AwError("source_request_session_mismatch")
            request = _Request(session)
            cleanup = self._loop.create_task(self._finish_request(request, owned))
            self._requests.add(cleanup)
            cleanup.add_done_callback(self._requests.discard)
        interrupted = False
        try:
            await session.prepare()
            if (
                not owned
                and not session.fallback
                and not self._confirm_routes(session.admission.routes)
            ):
                raise AwError("source_route_effect_unsupported")
            if self._state != "active":
                raise AwError("source_unavailable")
            token = self._request_context.set(request)
            try:
                yield session
            finally:
                self._request_context.reset(token)
        except (asyncio.CancelledError, TimeoutError):
            interrupted = True
            raise
        finally:
            with self._lock:
                request.ended = True
                for stream in request.sync_streams:
                    stream.schedule_close()
                self._check_drained(request)
            # Teardown retains pending cleanup past cancellation or the request deadline.
            done, _ = await asyncio.wait(
                {cleanup}, timeout=session.admission.timeout_ms / 1000
            )
            if not interrupted:
                if not done:
                    raise AwError("source_close_pending")
                if failure := cleanup.result():
                    raise failure

    async def _install_request_routes(self, routes: dict[str, Any]) -> bool:
        if not self._owns_source:
            await self._source.instance.installed(routes)
        return self._confirm_routes(routes)

    async def _finish_request(
        self, request: _Request, owned: bool
    ) -> BaseException | None:
        try:
            await request.drained.wait()
            if owned:
                try:
                    await request.session.close()
                except AwError as error:
                    if error.code != "source_close_pending":
                        raise
                    # Session.close keeps draining an already admitted SDK operation.
                    if failure := await request.session._closing:
                        raise failure
            if request.cleanup_error is not None:
                self._cleanup_unconfirmed = True
                return request.cleanup_error
        except (Exception, asyncio.CancelledError) as error:  # noqa: BLE001
            self._cleanup_unconfirmed = True
            return error
        return None

    def _call_request(self) -> _Request | None:
        with self._lock:
            request = self._request_context.get(None)
            if self._state == "active" or (
                self._state == "draining"
                and request is not None
                and request.session._native_owner() is request
            ):
                if request is None:
                    raise AwError("source_request_session_required")
                request.require_live()
                request.session._require_open()
                return request
            if self.availability == "required" and self._state in (
                "activating",
                "failed",
                "draining",
                "closed",
            ):
                raise AwError("source_unavailable")
            return None

    def _check_request(self, request: _Request) -> None:
        if self._request_context.get(None) is not request:
            raise AwError("source_request_session_mismatch")
        request.require_live()

    @contextmanager
    def _admit(
        self,
        request: _Request,
        seam: Seam,
        args: tuple[Any, ...],
        kwargs: dict[str, Any],
    ) -> Iterator[Invocation | None]:
        call = None
        with self._lock:
            self._check_request(request)
            current = self._call_request()
            if current is not None:
                if not current.session.fallback:
                    call = Invocation(
                        current.session,
                        seam.target,
                        CallFields(seam.signature, seam.receiver, args, kwargs),
                        self._session_id_context,
                    )
                request.calls += 1
        try:
            yield call
        finally:
            if call is not None and call.settled is not None:
                self._loop.call_soon_threadsafe(call.settled.set_result, None)
            with self._lock:
                if current is not None:
                    request.calls -= 1
                    self._check_drained(request)

    def _check_drained(self, request: _Request) -> None:
        if request.ended and request.calls == 0 and not request.sync_streams:
            self._loop.call_soon_threadsafe(request.drained.set)

    def _settle_stream(
        self, request: _Request, stream: _SyncStream, error: BaseException | None = None
    ) -> None:
        with self._lock:
            if stream not in request.sync_streams:
                return
            request.sync_streams.remove(stream)
            if error is not None and request.cleanup_error is None:
                request.cleanup_error = error
            self._check_drained(request)

    def _sync(self, hook: Coroutine[Any, Any, _Result]) -> _Result:
        loop = self._loop
        try:
            current = asyncio.get_running_loop()
        except RuntimeError:
            current = None
        if loop is current:
            hook.close()
            raise AwError("source_sync_call_on_activation_loop")
        if loop is None or not loop.is_running():
            hook.close()
            raise AwError("source_unavailable")
        try:
            pending = asyncio.run_coroutine_threadsafe(hook, loop)
        except RuntimeError:
            hook.close()
            raise AwError("source_unavailable") from None
        return pending.result()

    async def _async(self, hook: Coroutine[Any, Any, _Result]) -> _Result:
        if asyncio.get_running_loop() is not self._loop:
            hook.close()
            raise AwError("source_activation_loop_required")
        return await hook

    async def teardown(self, timeout_ms: int = 5000) -> None:
        """Gate new calls, drain admitted requests, then restore targets and close AW."""
        if type(timeout_ms) is not int or not 1 <= timeout_ms <= 60000:
            raise AwError("invalid_attach_timeout")
        deadline = monotonic() + timeout_ms / 1000
        loop = asyncio.get_running_loop()
        with self._lock:
            if self._loop is not None and self._loop is not loop:
                raise AwError("source_activation_loop_required")
            if self._drain_task is None:
                self._loop = loop
                self._state = "draining"
                self._drain_task = loop.create_task(self._drain(timeout_ms))
        done, _ = await asyncio.wait(
            {self._drain_task}, timeout=max(0, deadline - monotonic())
        )
        if not done:
            with self._lock:
                if self.availability == "required":
                    self._poisoned = self._cleanup_unconfirmed = True
                    raise AwError("source_cleanup_unconfirmed")
            return
        failure = self._drain_task.result()
        if self.availability == "required" and (
            failure is not None or self._cleanup_unconfirmed
        ):
            raise AwError("source_cleanup_unconfirmed") from failure

    async def _drain(self, timeout_ms: int) -> BaseException | None:
        if self._requests:
            await asyncio.wait(self._requests.copy())
        with self._lock:
            self._remove_finder()
            failure = self._restore()
        if self._sync_closer is not None:
            self._sync_closer.shutdown()
        budget = max(100, timeout_ms)
        try:
            if self._owns_source:
                await self._source.close(timeout_ms=budget)
        except (Exception, asyncio.CancelledError) as error:  # noqa: BLE001
            failure = failure or error
        finally:
            with self._lock:
                self._state = "closed"
                self._poisoned |= (
                    failure is not None and self.availability == "required"
                )
        return failure
