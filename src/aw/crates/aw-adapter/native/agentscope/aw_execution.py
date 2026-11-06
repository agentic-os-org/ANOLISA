"""Application-owned private execution using the shared lease contract."""

import asyncio

from agentscope.tool import ToolChunk, ToolResponse
from agentscope.middleware import MiddlewareBase

from aw_private import clear_execution, current_execution, validate_execution
from aw_scope_ledger import CallbackFailure

LEASE_TIMEOUT = 60


def public_value(value):
    lease = current_execution.get()
    if lease is None:
        return value
    try:
        return lease.redact(value)
    except Exception:
        raise CallbackFailure("aw_private_redaction_failed") from None


class AwExecution:
    """Retain lease ownership independently of reply and cleanup task contexts."""

    def __init__(self, factory):
        self.factory = factory
        self.manager = self.lease = None
        self.entered = self.failed = False

    async def acquire(self):
        async def enter():
            self.manager = self.factory()
            self.lease = await self.manager.__aenter__()
            self.entered = True
            validate_execution(self.lease)

        self.pending = asyncio.create_task(enter())
        done, _ = await asyncio.wait({self.pending}, timeout=LEASE_TIMEOUT)
        if not done:
            self.failed = True
            opening = self.pending

            async def retire_late():
                try:
                    await opening
                finally:
                    if self.entered:
                        await self._revoke()

            self.pending = asyncio.create_task(retire_late())
            self.pending.add_done_callback(self._consume)
            raise CallbackFailure("aw_private_acquisition_timeout_restart_worker")
        try:
            self.pending.result()
        except asyncio.CancelledError:
            raise
        except Exception:
            raise CallbackFailure("aw_private_acquisition_failed") from None

    @staticmethod
    def _consume(task):
        if not task.cancelled():
            task.exception()

    async def _revoke(self):
        try:
            await self.manager.__aexit__(None, None, None)
        finally:
            self.entered = False
            clear_execution(self.lease)

    async def revoke(self):
        if self.failed:
            raise CallbackFailure("aw_private_cleanup_failed_restart_worker")
        if not self.entered:
            return
        self.pending = asyncio.create_task(self._revoke())
        self.pending.add_done_callback(self._consume)
        done, _ = await asyncio.wait({self.pending}, timeout=LEASE_TIMEOUT)
        try:
            if not done:
                raise TimeoutError()
            self.pending.result()
        except BaseException:
            # Keep the original task and reservation after unknown revocation.
            self.failed = True
            raise CallbackFailure("aw_private_cleanup_failed_restart_worker") from None


class AwPrivateMiddleware(MiddlewareBase):
    """Redact complete native results before public tool Hooks and delivery."""

    async def on_acting(self, agent, input_kwargs, next_handler):
        try:
            async for item in next_handler():
                # A secret can span native chunks; only publish the final result.
                if isinstance(item, ToolChunk):
                    continue
                if isinstance(item, ToolResponse):
                    item = ToolResponse.model_validate(public_value(item.model_dump(mode="json")))
                yield item
        except asyncio.CancelledError:
            raise
        except Exception:
            raise CallbackFailure("aw_private_tool_failed") from None
