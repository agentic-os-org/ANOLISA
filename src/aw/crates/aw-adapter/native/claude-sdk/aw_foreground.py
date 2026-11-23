"""Checked foreground clients serialize submissions and retain native response ownership."""

from contextlib import aclosing
from pathlib import Path
import asyncio
import os
from uuid import uuid4

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
        self._aw_observe_sessions = any(
            hook["event"] in {"session.start", "session.end"} for hook in bridge.config["hooks"]
        )
        self._aw_input_hook = next(
            (hook for hook in bridge.config["hooks"] if hook["event"] == "input.submit"), None
        )

    async def connect(self, prompt=None):
        if self._aw_state != "new":
            raise RuntimeError("AW checked foreground clients cannot reconnect")
        self._aw_state = "opening"
        try:
            await super().connect(None)
            self._aw_bridge.bind_stop_deliveries(getattr(self, "_query", None))
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
            native = prompt
            turn_id = str(uuid4())
            if self._aw_input_hook is not None:
                from aw_submission import SubmissionDenied

                native, payload = await prepare(
                    prompt, session_id, self._aw_cwd, self._aw_input_hook["budget_ms"]
                )
                try:
                    await check(self._aw_bridge, payload)
                except (SubmissionDenied, asyncio.CancelledError):
                    raise
                except Exception:
                    self._aw_failed = True
                    raise
                turn_id = payload["aw_turn_id"]
            self._aw_pending = True
            self._aw_bridge.turn_id = turn_id
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
                    if self._aw_observe_sessions:
                        await self._aw_bridge.session_initialized(message, self._aw_cwd)
                    if isinstance(message, ResultMessage):
                        self._aw_pending = False
                        self._aw_bridge.turn_id = None
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
            if self._aw_observe_sessions:
                await self._aw_bridge.session_disconnected(self._aw_cwd, "disconnected")
        self._aw_state = "closed"
