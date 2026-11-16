"""Keep an admitted native bridge valid through model, tool and cleanup waits."""

import asyncio
import os
import re
from typing import TYPE_CHECKING, Callable

from aw_sdk.transport import AwError

if TYPE_CHECKING:
    from aw_sdk.client import Task


class ScopeWatch:
    """Renew one immutable Scope and latch bridge failures until native drain."""

    def __init__(self, task: "Task", probe: Callable | None, on_failure: Callable | None) -> None:
        self.task, self.probe, self.on_failure = task, probe, on_failure
        self.error: Exception | None = None
        self.closed = False
        self.monitor: asyncio.Task | None = None
        self.policy_revision: str | None = None

    async def start(self) -> None:
        watch = await self.call("watch_scope", pid=os.getpid())
        if (
            not isinstance(watch, dict)
            or not isinstance(watch.get("policy_revision"), str)
            or re.fullmatch(r"[a-f0-9]{64}", watch["policy_revision"]) is None
            or watch.get("interval_ms") != 2000
        ):
            raise AwError("scope_watch_invalid")
        self.policy_revision = watch["policy_revision"]
        self.monitor = asyncio.create_task(self.renew(watch["interval_ms"]))

    async def call(self, method: str, **fields: object) -> dict:
        return await self.task.call(
            {
                "method": method,
                "instance_id": self.task.binding["instance_id"],
                **fields,
            }
        )

    def check(self) -> None:
        if self.error is not None:
            raise self.error
        if self.closed:
            raise AwError("scope_watch_closed")

    async def validate(self) -> None:
        failure = None
        try:
            self.check()
            if self.probe is not None:
                self.probe()
        except Exception as error:
            failure = error
        try:
            await self.call(
                "check_scope",
                pid=os.getpid(),
                policy_revision=self.policy_revision,
                valid=failure is None,
            )
            if failure is not None:
                raise failure
            self.check()
        except Exception as error:
            if self.error is None and not self.closed and not self.task._closing:
                self.error = error
                if self.on_failure is not None:
                    self.on_failure(error)
            raise

    async def renew(self, interval_ms: int) -> None:
        try:
            while not self.closed:
                await asyncio.sleep(interval_ms / 1000)
                await self.validate()
        except asyncio.CancelledError:
            raise
        except Exception:
            pass  # validate latched the failure and notified the native owner.

    async def close(self) -> None:
        self.closed = True
        if self.monitor is not None:
            self.monitor.cancel()
            await asyncio.gather(self.monitor, return_exceptions=True)
