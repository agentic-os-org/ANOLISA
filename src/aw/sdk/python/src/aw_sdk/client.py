"""Application-owned registration and immutable per-task availability."""

import asyncio
from copy import deepcopy
import inspect
import os
from pathlib import Path
from typing import Any, Callable
from uuid import uuid4

from aw_sdk.transport import AwError, SDK, rpc, same_identity
from aw_sdk.scope_watch import ScopeWatch


class Instance:
    """Connect installed callbacks to AW without starting either process."""

    def __init__(
        self,
        *,
        endpoint: str,
        target: str,
        capabilities: dict,
        availability: str,
        install: Callable,
        cwd: str | None = None,
        environment: dict[str, str] | None = None,
        config: str | None = None,
        timeout_ms: int = 1000,
        reconnect_ms: int = 1000,
    ) -> None:
        if availability not in {"required", "native_fallback"}:
            raise AwError("availability_required")
        if not callable(install):
            raise AwError("ready_confirmation_required")
        if type(reconnect_ms) is not int or not 100 <= reconnect_ms <= 60000:
            raise AwError("invalid_reconnect_interval")
        self.endpoint, self.target = endpoint, target
        self.capabilities = deepcopy(capabilities)
        self._availability, self.install = availability, install
        self.cwd = str(Path(cwd or os.getcwd()).resolve(strict=True))
        self.environment = dict(os.environ if environment is None else environment)
        self.config, self.timeout_ms, self.reconnect_ms = config, timeout_ms, reconnect_ms
        self.state = "detached"
        self.last_error: Exception | None = None
        self.connection: dict | None = None
        self.pending: dict | None = None
        self.retired: dict | None = None
        self.tasks: set[Task] = set()
        self._connecting: asyncio.Task | None = None
        self._loop: asyncio.Task | None = None
        self.closed = False
        self.controller = None
        self.control_key = None
        self.confirm_control = None
        self.application_id: str | None = None
        self.detaching = False

    @property
    def availability(self) -> str:
        return self._availability

    async def installed(self, routes: dict) -> None:
        ready = self.install(routes)
        if inspect.isawaitable(ready):
            ready = await ready
        if ready is not True:
            raise AwError("ready_not_confirmed")

    async def connect(self) -> bool:
        if self.closed:
            raise AwError("instance_closed")
        if self.state == "ready":
            return True
        if self._connecting is None:
            self._connecting = asyncio.create_task(self._prepare())
        pending = self._connecting
        try:
            return await asyncio.shield(pending)
        finally:
            if pending.done() and self._connecting is pending:
                self._connecting = None

    async def _prepare(self) -> bool:
        try:
            discovery = await rpc(self.endpoint, None, {"method": "status"}, self.timeout_ms)
            if discovery["result"].get("sdk_protocol") != SDK:
                raise AwError("sdk_protocol_version")
            identity = discovery["identity"]
            if self.retired and same_identity(self.retired["identity"], identity):
                await self._registration(
                    identity, {"action": "release", "handle": self.retired["handle"]}
                )
                self.retired["released"] = True
            self.retired = None
            recovered = self.pending and same_identity(self.pending["identity"], identity)
            if not recovered:
                ticks = Path("/proc/self/stat").read_text().rsplit(")", 1)[1].split()[19]
                self.pending = {
                    "identity": identity,
                    "key": {
                        "request_id": str(uuid4()),
                        "owner": {"pid": os.getpid(), "start_ticks": ticks},
                    },
                }
            key = self.pending["key"]
            command = (
                {"action": "query", "key": key}
                if recovered
                else {
                    "action": "register_sdk",
                    "sdk_version": SDK,
                    "availability": self.availability,
                    "key": key,
                    "target": self.target,
                    "capabilities": self.capabilities,
                    "cwd": self.cwd,
                    "environment": self.environment,
                    **({"config": self.config} if self.config is not None else {}),
                }
            )
            try:
                result = await self._registration(identity, command)
            except AwError as error:
                if recovered and error.code == "registration_unknown":
                    self.pending = None
                    return False
                raise
            state = result.get("state")
            if state == "preparing":
                return False
            if state in {"released", "expired"}:
                self.pending = None
                return False
            if state == "failed":
                raise AwError(result.get("error") or "registration_failed")
            if state not in {"pending_ready", "ready"}:
                raise AwError("registration_unavailable")
            self.pending["handle"] = result["handle"]
            await self.installed(result["routes"])
            if self.closed:
                raise AwError("instance_closed")
            ready = await self._registration(
                identity,
                {
                    "action": "ready",
                    "handle": result["handle"],
                    "policy_revision": result["policy_revision"],
                },
            )
            if ready.get("state") != "ready":
                raise AwError("ready_not_confirmed")
            connection = {
                "identity": identity,
                "handle": result["handle"],
                "routes": result["routes"],
            }
            if self.confirm_control:
                await self.confirm_control(connection)
            self.connection = connection
            self.pending = None
            self.state, self.last_error = "ready", None
            return True
        except Exception as error:
            self.state, self.last_error = "unavailable", error
            if (
                self.availability == "native_fallback"
                and isinstance(error, AwError)
                and error.unavailable
            ):
                return False
            raise

    async def _registration(self, identity: dict, command: dict) -> dict:
        response = await rpc(
            self.endpoint, identity, {"method": "registration", "command": command}, self.timeout_ms
        )
        return response["result"]

    def listen(self, *, automatic: bool = False) -> None:
        """Publish cooperative support; automatic attachment is explicitly selected."""
        from aw_sdk.control import Controller

        if self.closed:
            raise AwError("instance_closed")
        if self.controller and not self.controller.finished:
            raise AwError("sdk_control_already_listening")
        if self._connecting and not self._connecting.done():
            raise AwError("sdk_control_busy")
        if self._loop:
            self._loop.cancel()
        self.controller = Controller(self, automatic)

    async def detach_control(self) -> None:
        if any(not task.closed and not task.fallback for task in self.tasks):
            raise AwError("sdk_control_busy")
        self.detaching = True
        try:
            connection = (
                self.connection
                or self.retired
                or (self.pending if self.pending and "handle" in self.pending else None)
            )
            if connection and (
                not self.controller
                or not self.controller.identity
                or same_identity(connection["identity"], self.controller.identity)
            ):
                await self._registration(
                    connection["identity"], {"action": "release", "handle": connection["handle"]}
                )
            self.connection = self.retired = self.pending = None
            self.state, self.last_error = "detached", None
        finally:
            self.detaching = False

    def start(self) -> None:
        if self.controller and not self.controller.finished:
            raise AwError("sdk_control_already_listening")
        if self.closed:
            raise AwError("instance_closed")
        if self._loop is None or self._loop.done():
            self._loop = asyncio.create_task(self._reconnect())

    async def _reconnect(self) -> None:
        while not self.closed:
            try:
                await self.connect()
            except Exception as error:
                self.last_error = error
                if not isinstance(error, AwError) or not error.unavailable:
                    return
            await asyncio.sleep(self.reconnect_ms / 1000)

    async def stop(self) -> None:
        if self.controller:
            await self.controller.stop()
        if self._loop is not None:
            self._loop.cancel()
            await asyncio.gather(self._loop, return_exceptions=True)
            self._loop = None

    def lost(self, connection: dict, error: AwError | None = None) -> None:
        error = error or AwError("ECONNRESET", unavailable=True)
        if self.connection is connection:
            self.state, self.retired, self.connection = "unavailable", connection, None
        for task in self.tasks:
            if task.connection is connection:
                task.connection_error = error
                task.fallback = self.availability == "native_fallback"

    async def task(
        self,
        *,
        cwd: str | None = None,
        environment: dict[str, str] | None = None,
    ) -> "Task":
        if self.detaching:
            raise AwError("sdk_control_busy")
        if self.closed:
            raise AwError("instance_closed")
        if self.state != "ready" and self.availability == "required":
            raise AwError("instance_not_ready")
        if self.last_error and not (
            isinstance(self.last_error, AwError) and self.last_error.unavailable
        ):
            raise self.last_error
        task = Task(self, self.connection, cwd=cwd, environment=environment)
        self.tasks.add(task)
        try:
            await task.open()
            return task
        except BaseException:
            try:
                await task.close()
            except Exception as cleanup_error:
                self.last_error = cleanup_error
            raise

    async def close(self) -> None:
        self.closed = True
        await self.stop()
        if self._connecting:
            await asyncio.gather(self._connecting, return_exceptions=True)
            self._connecting = None
        await asyncio.gather(*(task.close() for task in list(self.tasks)))
        connection = (
            self.connection
            or self.retired
            or (self.pending if self.pending and "handle" in self.pending else None)
        )
        if connection:
            try:
                await self._registration(
                    connection["identity"], {"action": "release", "handle": connection["handle"]}
                )
            except AwError as error:
                if not error.unavailable:
                    raise
        self.connection, self.state = None, "closed"

    async def __aenter__(self) -> "Instance":
        await self.connect()
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.close()


class Task:
    """A fixed policy snapshot; a fallback task never rejoins a recovered service."""

    def __init__(
        self,
        instance: Instance,
        connection: dict | None,
        *,
        cwd: str | None = None,
        environment: dict[str, str] | None = None,
    ) -> None:
        self.instance, self.connection = instance, connection
        self.cwd = str(Path(instance.cwd if cwd is None else cwd).resolve(strict=True))
        self.environment = dict(instance.environment if environment is None else environment)
        self.fallback = connection is None
        self.routes: dict = {"events": {}, "hooks": []}
        self.binding: dict | None = None
        self.closed = False
        self._closing: asyncio.Task | None = None
        self.connection_error: AwError | None = None
        self.watch: ScopeWatch | None = None

    async def call(self, operation: dict, timeout_ms: int | None = None) -> dict | None:
        if self.closed or self._closing:
            raise AwError("task_closed")
        if self.fallback:
            return None
        if self.connection_error:
            raise self.connection_error
        try:
            response = await rpc(
                self.instance.endpoint,
                self.connection["identity"],
                operation,
                self.instance.timeout_ms if timeout_ms is None else timeout_ms,
            )
            if self.connection_error and not self.fallback:
                raise self.connection_error
            return None if self.fallback else response["result"]
        except AwError as error:
            if error.unavailable:
                self.instance.lost(self.connection, error)
                if self.fallback:
                    return None
            raise

    async def open(self) -> None:
        if self.fallback:
            return
        self.binding = await self.call(
            {
                "method": "open_scope",
                "parent_id": self.connection["handle"]["binding"]["instance_id"],
                "pid": os.getpid(),
                "cwd": self.cwd,
                "environment": self.environment,
            }
        )
        if self.binding is None:
            return
        instance_id = self.binding["instance_id"]
        self.routes = (
            await self.call({"method": "scope_policy", "instance_id": instance_id}) or self.routes
        )
        if not self.fallback:
            await self.instance.installed(self.routes)
            await self.call({"method": "acknowledge_scope", "instance_id": instance_id})

    async def watch_scope(
        self,
        *,
        probe: Callable | None = None,
        on_failure: Callable | None = None,
    ) -> ScopeWatch:
        """Renew native bridge validity until this Task closes after native drain."""
        if (
            self.instance.availability != "required"
            or self.fallback
            or self.binding is None
            or self.watch is not None
        ):
            raise AwError("scope_watch_unavailable")
        self.watch = ScopeWatch(self, probe, on_failure)
        await self.watch.start()
        return self.watch

    async def invoke_native(self, hook: dict, payload: dict) -> tuple[int, bytes, bytes]:
        try:
            return await self._invoke_step(hook, payload)
        except BaseException:
            await self.abort_event(hook, payload)
            raise

    async def abort_event(self, hook: dict, payload: dict) -> None:
        # A separate task keeps drain alive even if the caller is cancelled again.
        cleanup = asyncio.create_task(self.close_event(hook, payload))
        while not cleanup.done():
            try:
                await asyncio.shield(cleanup)
            except asyncio.CancelledError:
                continue
            except Exception:
                break
        try:
            cleanup.result()
        except Exception as error:
            self.instance.last_error = error

    async def close_event(self, hook: dict, payload: dict) -> None:
        """Drain an interrupted event using its original Scope and correlation."""
        if self._closing:
            await asyncio.shield(self._closing)
            return
        if not self.binding or self.closed or self.fallback or self.connection_error:
            return
        await self.call(
            {
                "method": "sdk_callback",
                "instance_id": self.binding["instance_id"],
                "event": hook["event"],
                "step_id": hook["step"],
                "native_input": payload,
                "close": True,
            }
        )

    async def _invoke_step(self, hook: dict, payload: dict) -> tuple[int, bytes, bytes]:
        if self.closed or self._closing:
            raise AwError("task_closed")
        if self.fallback:
            return 0, b"", b""
        result = await self.call(
            {
                "method": "sdk_callback",
                "instance_id": self.binding["instance_id"],
                "event": hook["event"],
                "step_id": hook["step"],
                "native_input": payload,
                "close": False,
            },
            max(self.instance.timeout_ms, hook.get("budget_ms", 0) + 1000),
        )
        if result is None:
            return 0, b"", b""
        if type(result.get("code")) is not int or result["code"] not in (0, 2):
            raise AwError("invalid_callback_response")
        outputs = []
        for key in ("stdout", "stderr"):
            value = result.get(key)
            if (
                not isinstance(value, list)
                or len(value) > 1024 * 1024
                or any(type(n) is not int or not 0 <= n <= 255 for n in value)
            ):
                raise AwError("invalid_callback_response")
            outputs.append(bytes(value))
        return result["code"], outputs[0], outputs[1]

    async def emit(self, event: str, payload: dict) -> dict[str, str]:
        """Emit at an installed boundary; the caller adopts block before native work."""
        if self.closed or self._closing:
            raise AwError("task_closed")
        if self.instance.capabilities.get("adapter") != "sdk":
            raise AwError("semantic_profile_required")
        if event not in self.instance.capabilities["events"]:
            raise AwError("semantic_event_unavailable")
        native = {**deepcopy(payload), "api_version": "aw-hook-source/v1", "event": event}
        hooks = [hook for hook in self.routes["hooks"] if hook["event"] == event]
        for hook in hooks:
            code, _, _ = await self.invoke_native(hook, native)
            if code == 2:
                return {"decision": "block", "coverage": "checked"}
            if self.fallback:
                break
        return {
            "decision": "continue",
            "coverage": (
                "unavailable" if self.fallback else "checked" if hooks else "not_configured"
            ),
        }

    async def close(self) -> None:
        if self.closed:
            return
        if self._closing is None:
            self._closing = asyncio.create_task(self._release())
        closing = self._closing
        try:
            await asyncio.shield(closing)
        finally:
            if closing.done() and self._closing is closing:
                self._closing = None

    async def _release(self) -> None:
        if self.watch is not None:
            await self.watch.close()
        if self.binding and not self.connection.get("released"):
            try:
                await rpc(
                    self.instance.endpoint,
                    self.connection["identity"],
                    {"method": "release_instance", "instance_id": self.binding["instance_id"]},
                    self.instance.timeout_ms,
                )
            except AwError as error:
                if not (self.fallback or self.connection_error) or not (
                    error.unavailable or error.code == "stale_service"
                ):
                    raise
        self.closed = True
        self.instance.tasks.discard(self)

    async def __aenter__(self) -> "Task":
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.close()
