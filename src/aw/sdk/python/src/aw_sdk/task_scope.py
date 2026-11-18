"""Carry a resident producer Scope over one admitted SDK Task.

Managed resident bridges reach the Service through the ``aw scope`` command. A
cooperative worker uses the same producer contract over its SDK connection, so
the native bridge keeps its invocation ledger, drain and result adoption while
the SDK owns admission, acknowledgement, renewal and release.
"""

import os
from typing import Any, Callable

from aw_sdk.admission import Admission
from aw_sdk.client import Instance

__all__ = ["TaskScope"]


class TaskScope:
    """The installed native client owns this Task until its callbacks drain."""

    def __init__(
        self,
        instance: Instance,
        timeout_ms: int,
        install: Callable,
        *,
        before_open: Callable[[], None] | None = None,
    ) -> None:
        self.instance = instance
        self.timeout_ms = timeout_ms
        self.install = install
        self.before_open = before_open
        self.admission: Admission | None = None
        self.handle: dict | None = None
        self.closed = False
        self.failed = False
        self.on_failure: Callable[[BaseException], None] | None = None
        self.watch = None
        self.invocation_limit = 0
        self.submission_sequence: int | None = None
        self.pid = os.getpid()

    def check(self, *, cleanup: bool = False) -> None:
        if self.pid != os.getpid() or self.closed or self.handle is None:
            raise RuntimeError("SDK scope is closed, unbound or inherited across fork")
        if not cleanup:
            if self.failed:
                raise RuntimeError("AW scope bridge validity failed")
            if self.watch is not None:
                self.watch.check()

    async def open(
        self, cwd: str, environment: dict[str, str], *, acknowledge: bool = True
    ) -> None:
        if self.before_open is not None:
            self.before_open()
        self.admission = Admission(
            self.instance,
            events=list(self.instance.capabilities["events"]),
            timeout_ms=self.timeout_ms,
            cwd=cwd,
            environment=environment,
            install=self.install,
        )
        await self.admission.prepare()
        self.handle = {"binding": self.admission.task.binding}
        if acknowledge:
            await self.acknowledge()

    async def call(self, method: str, **fields: Any) -> dict:
        return await self.admission.task.call(
            {"method": method, "instance_id": self.handle["binding"]["instance_id"], **fields}
        )

    async def acknowledge(self) -> None:
        self.check()
        limits = await self.call("enable_invocations")
        self.invocation_limit = limits["invocations_per_binding"]
        self.watch = await self.admission.task.watch_scope(on_failure=self.invalidate)
        if self.watch.policy_revision != self.admission.routes["policy_revision"]:
            raise RuntimeError("AW Scope revision changed during admission")

    def invalidate(self, error: BaseException) -> None:
        if not self.failed:
            self.failed = True
            if self.on_failure is not None:
                self.on_failure(error)

    async def validate(self) -> None:
        self.check()
        await self.watch.validate()

    async def next_submission(self) -> str:
        self.check()
        if self.submission_sequence is None:
            await self.call("enable_submissions")
            self.submission_sequence = 0
        if self.submission_sequence >= (1 << 64) - 1:
            raise RuntimeError("AW submission sequence exhausted")
        self.submission_sequence += 1
        return str(self.submission_sequence)

    async def invoke_native(
        self, hook: dict, payload: dict, *, drain_on_failure: bool = True
    ) -> tuple[int, bytes, bytes]:
        """Run one step; without ``drain_on_failure`` the caller closes the event."""
        task = self.admission.task
        if hook.get("close_event"):
            await task.close_event(hook, payload)
            return 0, b"", b""
        await self.validate()
        if not drain_on_failure:
            return await task._invoke_step(hook, payload)
        return await task.invoke_native(hook, payload)

    async def register(self, sequence: int, call_id: str, name: str, arguments: Any) -> None:
        await self.validate()
        await self.call(
            "register_invocation", sequence=sequence, call_id=call_id, tool_name=name, input=arguments
        )

    async def finish(self, sequence: int, call_id: str) -> None:
        self.check(cleanup=True)
        await self.call("finish_invocation", sequence=sequence, call_id=call_id)

    async def close(self) -> None:
        self.closed = True
        if self.admission is not None:
            await self.admission.close()
