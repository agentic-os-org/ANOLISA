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
import uuid
from typing import Any, AsyncGenerator, Callable

from agentscope.message import TextBlock, ToolResultState
from agentscope.middleware import MiddlewareBase
from agentscope.tool import ToolChunk, ToolResponse



def validate_hooks(config: dict[str, Any]) -> None:
    """Apply the same native route contract at launch and task handoff."""
    if not isinstance(config.get("hooks"), list):
        raise ValueError("AW plugin configuration requires hooks")
    for hook in config["hooks"]:
        if not isinstance(hook, dict):
            raise ValueError("AW Hook route must be an object")
        if hook.get("event") not in {"tool.before", "tool.after"}:
            raise ValueError("Unsupported AW native event")
        if not isinstance(hook.get("step"), str) or not hook["step"]:
            raise ValueError("AW step must be a nonempty string")
        if hook.get("on_error") not in {"report", "block", "withhold_result"}:
            raise ValueError("Invalid AW failure policy")
        if type(hook.get("budget_ms")) is not int or not 1 <= hook["budget_ms"] <= 55000:
            raise ValueError("AW native budget must be 1..55000 ms")
        if type(hook.get("replace_result", False)) is not bool:
            raise ValueError("AW result replacement must be boolean")
        if hook.get("replace_result", False) and hook["event"] != "tool.after":
            raise ValueError("AW result replacement requires tool.after")


class CallbackFailure(RuntimeError):
    """A fixed adapter failure code, separate from a native policy denial."""


class AwToolMiddleware(MiddlewareBase):
    """Observe completed native responses or deny entry with the AW exit-2 convention."""

    def __init__(self, config: dict[str, Any], hook: dict[str, Any]) -> None:
        self.config = config
        self.hook = hook
        self.scope = None

    async def _invoke(self, payload: dict[str, Any]) -> tuple[int, bytes, bytes]:
        if self.scope is not None:
            from .aw_scope import ScopeTransportFailure
            try:
                return await self.scope.invoke_native(self.hook, payload)
            except ScopeTransportFailure as error:
                raise CallbackFailure("scope_transport_failure") from error
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
            if self.hook["event"] == "tool.after" and self.hook["on_error"] == "withhold_result":
                raise CallbackFailure("strict_posttool_failed") from error
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
            directive.get("action") in ("ask", "approve")
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
        if self.hook["event"] == "tool.after" and isinstance(directive, dict) and "aw_replacement" in directive:
            try:
                return ToolResponse.model_validate(directive["aw_replacement"])
            except (ValueError, TypeError) as error:
                raise CallbackFailure("invalid_result_replacement") from error
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
                replacement = await self._run({**payload, "tool_response": item.model_dump(mode="json")})
                if replacement is not None:
                    item = replacement
            yield item


class AwReloadMiddleware(MiddlewareBase):
    """Track whole requests and actual tool generators independently of policy steps."""

    def __init__(self, config: dict[str, Any]) -> None:
        self.config = config

    async def lifecycle(self, phase: str, key: str) -> None:
        payload = json.dumps({"phase": phase, "key": key, "pid": os.getpid()}).encode()
        proc = await asyncio.create_subprocess_exec(
            self.config["executable"], "bridge", "--adapter", "qwenpaw",
            "--binding", self.config["binding"], stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
            start_new_session=True,
        )
        try:
            _, stderr = await asyncio.wait_for(proc.communicate(payload), timeout=3)
            if proc.returncode:
                raise CallbackFailure(f"lifecycle_{phase}_failed: {stderr.decode(errors='replace')}")
        finally:
            if proc.returncode is None:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                await proc.wait()

    async def on_reply(self, agent: Any, input_kwargs: dict[str, Any], next_handler: Callable[..., AsyncGenerator[Any, None]]) -> AsyncGenerator[Any, None]:
        key = str(uuid.uuid4())
        await self.lifecycle("request_start", key)
        generator = next_handler(**input_kwargs)
        finished = False
        try:
            async for item in generator:
                yield item
            finished = True
        finally:
            # AgentScope's wrapper does not propagate aclose to all children.
            # Only normal exhaustion proves completion; other exits stay busy.
            await generator.aclose()
            if finished:
                await self.lifecycle("request_end", key)

    async def on_acting(self, agent: Any, input_kwargs: dict[str, Any], next_handler: Callable[..., AsyncGenerator[Any, None]]) -> AsyncGenerator[Any, None]:
        import hashlib
        call = input_kwargs["tool_call"]
        session = agent.state.session_id
        # This identity need only be consistent within this middleware's lifetime.
        key = hashlib.sha256(json.dumps([session, call.id]).encode()).hexdigest()
        await self.lifecycle("tool_start", key)
        payload = {"session_id": session, "tool_call": call.model_dump(mode="json")}
        before = AwToolMiddleware(self.config, {"event": "tool.before", "step": "$aw.dispatch", "on_error": "block", "budget_ms": 55000})
        after = AwToolMiddleware(self.config, {"event": "tool.after", "step": "$aw.dispatch", "on_error": "report", "budget_ms": 55000})
        generator = None
        finished = False
        response = None
        terminal_chunk = None
        try:
            denied = await before._run({**payload, "event": "tool.before"})
            if denied is not None:
                replacement = await after._run({**payload, "event": "tool.after", "tool_response": denied.model_dump(mode="json")})
                finished = True
                yield replacement if replacement is not None else denied
                return
            generator = next_handler(**input_kwargs)
            # Callback failures do not prove that the native generator finished.
            while True:
                try:
                    item = await anext(generator)
                except StopAsyncIteration:
                    finished = True
                    break
                if response is not None or (terminal_chunk is not None and not isinstance(item, ToolResponse)):
                    raise CallbackFailure("native_output_after_final_response")
                if isinstance(item, ToolResponse):
                    replacement = await after._run({**payload, "event": "tool.after", "tool_response": item.model_dump(mode="json")})
                    # QwenPaw's Coordinator stops pulling at the final response.
                    # Exhaust the native stream before publishing that response.
                    response = replacement if replacement is not None else item
                elif isinstance(item, ToolChunk) and item.state in (ToolResultState.ERROR, ToolResultState.INTERRUPTED):
                    # Coordinator also stops on these chunks. Toolkit may still
                    # yield its accumulated response and await native cleanup.
                    terminal_chunk = item
                else:
                    yield item
        finally:
            if generator is not None:
                await generator.aclose()
            # A final response or wrapper closure cannot prove native cleanup.
            # Release only on normal exhaustion or denial before native entry.
            if finished:
                await self.lifecycle("tool_end", key)
        if terminal_chunk is not None:
            yield terminal_chunk
        if response is not None:
            yield response


class AwPlugin:
    """Validate once, then register factories which perform no configuration I/O."""

    def register(self, api: Any) -> None:
        if version("qwenpaw") != "2.2.2.post1" or version("agentscope") != "2.0.10":
            raise RuntimeError("AW requires QwenPaw 2.2.2.post1 and AgentScope 2.0.10")
        if api.config.get("enabled") is False:
            raise RuntimeError("QwenPaw configuration disables the AW plugin")
        descriptor = Path(__file__).with_name("aw-attachment.json")
        if descriptor.exists():
            # A hot-installed App cannot inherit environment added after startup.
            metadata = descriptor.lstat()
            if not descriptor.is_file() or descriptor.is_symlink() or metadata.st_uid != os.geteuid() or metadata.st_mode & 0o077:
                raise RuntimeError("Unsafe AW attachment descriptor")
            config = json.loads(descriptor.read_text())
        else:
            config = json.loads(Path(os.environ["AW_NATIVE_CONFIG"]).read_text())
        for key in ("executable", "binding", "ready_path", "ready_token"):
            if not isinstance(config.get(key), str) or not config[key]:
                raise ValueError(f"AW plugin configuration requires {key}")
        validate_hooks(config)
        if config.get("resident"):
            if config.get("reloadable"):
                raise RuntimeError("AW resident task scopes require a non-reloadable QwenPaw binding")
            from .aw_native import install_guard
            from .aw_resident import ResidentMiddleware
            install_guard()
            api.register_middleware(
                lambda ctx, agent_config: ResidentMiddleware(config, AwToolMiddleware, ctx),
                priority=0,
            )
        elif config.get("reloadable"):
            api.register_middleware(lambda ctx, agent_config: AwReloadMiddleware(config), priority=0)
        for hook in ([] if config.get("resident") else config["hooks"]):
            def factory(ctx: Any, agent_config: Any, hook: dict = hook) -> AwToolMiddleware:
                return AwToolMiddleware(config, hook)

            api.register_middleware(factory, priority=100)
        # The native App invokes startup hooks only after load_all_plugins has
        # completed and its plugin registry is published. Registration alone
        # would also succeed before the loader finishes or rolls a plugin back.
        api.register_startup_hook("aw-ready", lambda: self._startup(config) if config.get("reloadable") or config.get("resident_policy_reload") else self._ready(config))

    @staticmethod
    async def _startup(config: dict[str, Any]) -> None:
        if config.get("reloadable"):
            await AwReloadMiddleware(config).lifecycle("ready", "")
        if config.get("resident_policy_reload"):
            from .aw_scope import Scope
            await Scope({**config, "adapter": "qwenpaw"}).ready()
        AwPlugin._ready(config)

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
