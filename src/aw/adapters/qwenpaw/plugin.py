"""QwenPaw App middleware; native scheduling and permissions retain authority."""

from __future__ import annotations

import asyncio
from importlib.metadata import version
import json
import logging
import os
from pathlib import Path
import signal
import sys
import tempfile
from typing import Any, AsyncGenerator, Callable

from agentscope.message import TextBlock, ToolResultState
from agentscope.middleware import MiddlewareBase
from agentscope.tool import ToolResponse


class CallbackFailure(RuntimeError):
    """A fixed adapter failure code, separate from a native policy denial."""


class AwToolMiddleware(MiddlewareBase):
    """Observe completed native responses or deny entry with the AW exit-2 convention."""

    def __init__(self, config: dict[str, Any], hook: dict[str, Any]) -> None:
        self.config = config
        self.hook = hook

    async def _invoke(self, payload: dict[str, Any]) -> tuple[int, bytes, bytes]:
        encoded = json.dumps(payload, ensure_ascii=False, sort_keys=True).encode()
        if len(encoded) > 1024 * 1024:
            raise CallbackFailure("native_input_limit")
        proc = await asyncio.create_subprocess_exec(
            self.config["executable"],
            "hook",
            "--adapter", "qwenpaw",
            "--binding", self.config["binding"],
            "--event", self.hook["event"],
            "--step", self.hook["step"],
            "--on-error", self.hook["on_error"],
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            start_new_session=True,
        )
        try:
            stdout, stderr = await asyncio.wait_for(
                proc.communicate(encoded),
                timeout=(self.hook["budget_ms"] + 3000) / 1000,
            )
        except BaseException:
            if proc.returncode is None:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            await proc.wait()
            raise
        return proc.returncode, stdout, stderr

    async def _run(self, payload: dict[str, Any]) -> ToolResponse | None:
        try:
            return await self._evaluate(payload)
        except (OSError, TimeoutError, CallbackFailure) as error:
            code = str(error) if isinstance(error, CallbackFailure) else type(error).__name__
            logging.getLogger(__name__).error(
                "AW %s step %s failed: %s; on_error=%s",
                self.hook["event"], self.hook["step"], code, self.hook["on_error"],
            )
            if self.hook["event"] == "tool.before" and self.hook["on_error"] == "block":
                return ToolResponse(
                    state=ToolResultState.DENIED,
                    content=[TextBlock(text="AW hook failed; configured failure action blocked the tool")],
                )
            return None

    async def _evaluate(self, payload: dict[str, Any]) -> ToolResponse | None:
        code, stdout, stderr = await self._invoke(payload)
        if stderr:
            sys.stderr.buffer.write(stderr)
            sys.stderr.buffer.flush()
        try:
            directive = json.loads(stdout) if stdout.strip() else None
        except (ValueError, UnicodeDecodeError):
            directive = None
        if isinstance(directive, dict) and (
            directive.get("action") in {"ask", "approve"}
            or directive.get("decision") == "ask"
        ):
            raise CallbackFailure("unsupported_approval")
        if code == 2 and self.hook["event"] == "tool.before":
            reason = (stderr or stdout).decode(errors="replace").strip()
            return ToolResponse(
                state=ToolResultState.DENIED,
                content=[TextBlock(text=reason or "Denied by AW")],
            )
        if code != 0:
            raise CallbackFailure(f"native_exit_{code}")
        return None

    async def on_acting(
        self,
        agent: Any,
        input_kwargs: dict[str, Any],
        next_handler: Callable[..., AsyncGenerator[Any, None]],
    ) -> AsyncGenerator[Any, None]:
        """Leave onion nesting, streaming chunks and independent tool concurrency unchanged."""
        call = input_kwargs["tool_call"]
        session_id = agent.state.session_id
        payload = {
            "event": self.hook["event"],
            "session_id": session_id,
            "tool_call": call.model_dump(mode="json"),
        }
        if self.hook["event"] == "tool.before":
            denied = await self._run(payload)
            if denied is not None:
                yield denied
                return
        async for item in next_handler():
            # ToolChunk is intermediate; ToolResponse is the completed native result.
            if self.hook["event"] == "tool.after" and isinstance(item, ToolResponse):
                await self._run({**payload, "tool_response": item.model_dump(mode="json")})
            yield item


class AwPlugin:
    """Validate once, then register factories which perform no configuration I/O."""

    def register(self, api: Any) -> None:
        if version("qwenpaw") != "2.2.2b4" or version("agentscope") != "2.0.8":
            raise RuntimeError("AW requires QwenPaw 2.2.2b4 and AgentScope 2.0.8")
        if api.config.get("enabled") is False:
            raise RuntimeError("QwenPaw configuration disables the AW plugin")
        config = json.loads(Path(os.environ["AW_NATIVE_CONFIG"]).read_text())
        for key in ("executable", "binding", "ready_path", "ready_token"):
            if not isinstance(config.get(key), str) or not config[key]:
                raise ValueError(f"AW plugin configuration requires {key}")
        if not isinstance(config.get("hooks"), list) or not config["hooks"]:
            raise ValueError("AW plugin configuration requires hooks")
        for hook in config["hooks"]:
            if hook.get("event") not in {"tool.before", "tool.after"}:
                raise ValueError("Unsupported AW native event")
            if not isinstance(hook.get("step"), str) or not hook["step"]:
                raise ValueError("AW step must be a nonempty string")
            if hook.get("on_error") not in {"report", "block"}:
                raise ValueError("Invalid AW failure policy")
            if type(hook.get("budget_ms")) is not int or not 1 <= hook["budget_ms"] <= 55000:
                raise ValueError("AW native budget must be 1..55000 ms")
        for hook in config["hooks"]:
            def factory(ctx: Any, agent_config: Any, hook: dict = hook) -> AwToolMiddleware:
                return AwToolMiddleware(config, hook)

            api.register_middleware(factory, priority=100)
        # The native App invokes startup hooks only after load_all_plugins has
        # completed and its plugin registry is published. Registration alone
        # would also succeed before the loader finishes or rolls a plugin back.
        api.register_startup_hook("aw-ready", lambda: self._ready(config))

    @staticmethod
    def _ready(config: dict[str, Any]) -> None:
        path = Path(config["ready_path"])
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(
                mode="w", encoding="utf-8", dir=path.parent,
                prefix=".aw-ready-", delete=False,
            ) as output:
                temporary = Path(output.name)
                json.dump({
                    "version": 1, "adapter": "qwenpaw", "pid": os.getpid(),
                    "token": config["ready_token"], "hooks": len(config["hooks"]),
                }, output)
                output.write("\n")
            os.replace(temporary, path)
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)


plugin = AwPlugin()
