"""Checked foreground clients serialize submissions and retain native response ownership."""

from contextlib import aclosing
from pathlib import Path
import asyncio
import os

import anyio
from claude_agent_sdk import ClaudeSDKClient, ResultMessage
from aw_submission import check, prepare


class CheckedClient(ClaudeSDKClient):
    """Enable pre-dispatch text checks only when input.submit is configured."""

    def __init__(self, options, bridge):
        super().__init__(options=options)
        self._aw_bridge = bridge
        self._aw_cwd = str(Path(options.cwd or os.getcwd()).resolve())
        self._aw_state = "new"
        self._aw_input_task = None
        self._aw_pending = False
        self._aw_receiving = False
        self._aw_failed = False

    async def connect(self, prompt=None):
        if self._aw_state != "new":
            raise RuntimeError("AW checked foreground clients cannot reconnect")
        self._aw_state = "opening"
        try:
            await super().connect(None)
            self._aw_state = "connected"
        except BaseException:
            await self.disconnect()
            raise
        if prompt is not None:
            await self.query(prompt)

    async def query(self, prompt, session_id="default"):
        if (self._aw_state != "connected" or self._aw_pending
                or self._aw_input_task is not None or self._aw_receiving or self._aw_failed):
            raise RuntimeError("AW requires a usable client and a drained previous response")
        self._aw_input_task = asyncio.current_task()
        dispatched = False
        try:
            budget = next(h["budget_ms"] for h in self._aw_bridge.config["hooks"]
                          if h["event"] == "input.submit")
            native, payload = await prepare(prompt, session_id, self._aw_cwd, budget)
            try:
                await check(self._aw_bridge, payload)
            except asyncio.CancelledError:
                raise
            except Exception as error:
                from aw_submission import SubmissionDenied
                if not isinstance(error, SubmissionDenied):
                    self._aw_failed = True
                raise
            self._aw_pending = True
            dispatched = True
            await super().query(native, session_id)
        except BaseException:
            if dispatched:
                self._aw_failed = True
            raise
        finally:
            self._aw_input_task = None

    async def receive_messages(self):
        if (self._aw_state != "connected" or not self._aw_pending
                or self._aw_input_task is not None or self._aw_receiving or self._aw_failed):
            raise RuntimeError("AW requires one response consumer after input admission")
        self._aw_receiving = True
        try:
            async with aclosing(super().receive_messages()) as messages:
                async for message in messages:
                    if isinstance(message, ResultMessage):
                        self._aw_pending = False
                    yield message
        finally:
            self._aw_receiving = False

    async def receive_response(self):
        async with aclosing(self.receive_messages()) as messages:
            async for message in messages:
                yield message
                if isinstance(message, ResultMessage):
                    return

    async def disconnect(self):
        self._aw_state = "closing"
        with anyio.CancelScope(shield=True):
            producer = self._aw_input_task
            if producer is not None and producer is not asyncio.current_task():
                producer.cancel()
                async with asyncio.timeout(6):
                    await asyncio.gather(producer, return_exceptions=True)
            await super().disconnect()
        self._aw_state = "closed"
