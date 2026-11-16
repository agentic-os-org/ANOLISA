"""Bounded readiness and an immutable Task at an installed protection boundary."""

import asyncio
from copy import deepcopy

from aw_sdk.client import Instance, Task
from aw_sdk.transport import AwError


class Admission:
    """Sources call prepare before protected input or the first native tool."""

    def __init__(
        self,
        instance: Instance,
        *,
        events: list[str],
        timeout_ms: int = 5000,
        cwd: str | None = None,
        environment: dict[str, str] | None = None,
    ) -> None:
        if type(timeout_ms) is not int or not 100 <= timeout_ms <= 60000:
            raise AwError("invalid_attach_timeout")
        if not isinstance(events, list) or any(
            event not in instance.capabilities["events"] for event in events
        ):
            raise AwError("admission_event_unavailable")
        self.instance = instance
        self.events = list(dict.fromkeys(events))
        self.timeout_ms, self.cwd = timeout_ms, cwd
        self.environment = None if environment is None else dict(environment)
        self.task: Task | None = None
        self.preparing: asyncio.Task | None = None
        self.closing: asyncio.Task | None = None
        self.cancelled = False
        self.initial = {
            "events": {},
            "hooks": [{"event": event, "budget_ms": 55000 + timeout_ms} for event in self.events],
        }

    @property
    def fallback(self) -> bool:
        return self.task.fallback if self.task else False

    @property
    def routes(self) -> dict:
        return self.task.routes if self.task else deepcopy(self.initial)

    async def prepare(self) -> None:
        if self.cancelled:
            raise AwError("admission_closed")
        if self.preparing is None:
            self.preparing = asyncio.create_task(self._open())
        await asyncio.shield(self.preparing)

    async def _open(self) -> None:
        required = self.instance.availability == "required"
        if required or "input.submit" in self.events:
            deadline = asyncio.timeout(self.timeout_ms / 1000)
            try:
                async with deadline:
                    while self.instance.state != "ready":
                        if self.instance.closed:
                            raise AwError("instance_closed")
                        error = self.instance.last_error
                        if error is not None and not (
                            isinstance(error, AwError) and error.unavailable
                        ):
                            raise error
                        await asyncio.sleep(0.01)
            except TimeoutError:
                if required or not deadline.expired():
                    raise
        if self.cancelled:
            raise AwError("admission_closed")
        self.task = await self.instance.task(cwd=self.cwd, environment=self.environment)

    async def emit(self, event: str, payload: dict) -> dict[str, str]:
        await self.prepare()
        return await self.task.emit(event, payload)

    def cancel(self) -> None:
        self.cancelled = True
        if self.preparing and not self.preparing.done():
            self.preparing.cancel()

    async def close(self) -> None:
        if self.closing is None:
            self.closing = asyncio.create_task(self._close())
        await asyncio.shield(self.closing)

    async def _close(self) -> None:
        self.cancel()
        if self.preparing:
            await asyncio.gather(self.preparing, return_exceptions=True)
        if self.task:
            await self.task.close()
