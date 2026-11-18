"""Application-installed QwenPaw middleware using the public SDK task transport."""

import json
from typing import Any, AsyncGenerator, Callable
from uuid import uuid4

from agentscope.middleware import MiddlewareBase
from agentscope.tool import ToolResponse
from aw_sdk import AwError, Instance, Task
from .plugin import AwToolMiddleware, validate_hooks
from .aw_skill import skill_context, metadata as skill_metadata, verify as verify_skill
from .aw_submission import submission_text, SubmissionDenied


class Request:
    """Keep a reply Scope until its native tool generators have also drained."""

    def __init__(self, task: Task) -> None:
        self.task = task
        self.tools = 0
        self.reply_done = False

    async def release(self) -> None:
        if self.reply_done and self.tools == 0:
            await self.task.close()


class Evaluation(AwToolMiddleware):
    """Retain canonical response adoption while propagating SDK execution errors."""

    async def _invoke(self, payload: dict[str, Any]) -> tuple[int, bytes, bytes]:
        return await self.scope.invoke_native(self.hook, payload)

    async def _run(self, payload: dict[str, Any]) -> ToolResponse | dict | None:
        native = {**payload, **skill_metadata()}
        try:
            result = await self._evaluate(native)
            if (
                not self.scope.fallback
                and self.hook["event"] == "tool.before"
                and (result is None or isinstance(result, dict))
            ):
                verify_skill()
            return result
        except BaseException:
            await self.scope.abort_event(self.hook, native)
            raise


class QwenPawMiddleware(MiddlewareBase):
    """One immutable Scope per native reply; native tools are never replayed."""

    def __init__(self, instance: Instance) -> None:
        self.instance = instance
        self.config: dict = {"hooks": []}
        self.active: Request | None = None
        self.busy = False

    async def on_reply(
        self, agent: Any, input_kwargs: dict, next_handler: Callable
    ) -> AsyncGenerator[Any, None]:
        if self.busy:
            raise AwError("sdk_agent_busy")
        self.busy = True
        try:
            request = Request(await self.instance.task())
        except BaseException:
            self.busy = False
            raise
        self.active = request
        task = request.task
        stream = None
        try:
            turn_id = str(uuid4())
            for hook in task.routes["hooks"]:
                if hook["event"] != "input.submit" or task.fallback:
                    continue
                text = submission_text(input_kwargs.get("inputs"))
                if text is None:
                    continue
                payload = {
                    "event": "input.submit",
                    "session_id": agent.state.session_id,
                    "aw_turn_id": turn_id,
                    "input_text": text,
                }
                code, stdout, _ = await task.invoke_native(hook, payload)
                if code == 2:
                    raise SubmissionDenied("Input blocked by AW policy")
                if code != 0 or stdout.strip():
                    raise AwError("invalid_submission_response")
            stream = next_handler(**input_kwargs)
            async for item in stream:
                yield item
        finally:
            try:
                if stream is not None:
                    await stream.aclose()
            finally:
                self.active = None
                self.busy = False
                request.reply_done = True
                await request.release()

    async def on_acting(
        self, agent: Any, input_kwargs: dict, next_handler: Callable
    ) -> AsyncGenerator[Any, None]:
        request = self.active
        if request is None:
            raise AwError("sdk_task_required")
        task = request.task
        request.tools += 1
        # Keep task-local route metadata off shared middleware instances.
        active = Acting(task)
        stream = (
            next_handler(**input_kwargs)
            if task.fallback
            else active.run(agent, input_kwargs, next_handler)
        )
        try:
            async for item in stream:
                yield item
        finally:
            try:
                await stream.aclose()
            finally:
                request.tools -= 1
                await request.release()


class Acting:
    def __init__(self, task: Task) -> None:
        self.task = task
        self.config = {"hooks": task.routes["hooks"]}

    async def evaluate(self, event: str, payload: dict) -> ToolResponse | dict | None:
        result = None
        payload = dict(payload)
        hooks = [hook for hook in self.config["hooks"] if hook["event"] == event]
        if event == "tool.after":
            hooks.reverse()
        for hook in hooks:
            if self.task.fallback:
                break
            middleware = Evaluation(self.config, hook)
            middleware.scope = self.task
            current = await middleware._run(payload)
            if current is not None:
                result = current
                if event == "tool.before" and isinstance(current, ToolResponse):
                    return current
                if event == "tool.after":
                    payload["tool_response"] = current.model_dump(mode="json")
        return None if self.task.fallback else result

    @skill_context
    async def run(
        self, agent: Any, input_kwargs: dict, next_handler: Callable
    ) -> AsyncGenerator[Any, None]:
        call = input_kwargs["tool_call"]
        payload = {
            "event": "tool.before",
            "session_id": agent.state.session_id,
            "tool_call": call.model_dump(mode="json"),
        }
        before = await self.evaluate("tool.before", payload)
        if isinstance(before, ToolResponse):
            yield before
            return
        if isinstance(before, dict):
            call.input = json.dumps(before["aw_updated_input"], ensure_ascii=False, allow_nan=False)
        stream = next_handler(**input_kwargs)
        try:
            async for item in stream:
                if isinstance(item, ToolResponse):
                    after = await self.evaluate(
                        "tool.after",
                        {
                            **payload,
                            "event": "tool.after",
                            "tool_call": call.model_dump(mode="json"),
                            "tool_response": item.model_dump(mode="json"),
                        },
                    )
                    if after is not None:
                        item = after
                yield item
        finally:
            await stream.aclose()


class QwenPawPlugin:
    """Install through the native PluginAPI before the App startup hook runs."""

    @staticmethod
    def from_config(config_file: str, **options: Any):
        """Select the first protection boundary from the canonical AW configuration."""
        from .configured import from_config

        return from_config(config_file, **options)

    def __init__(self, instance: Instance, *, automatic: bool = False) -> None:
        self.instance = instance
        self.automatic = automatic
        self.registered = False

    def ready(self, routes: dict) -> bool:
        validate_hooks(routes)
        return self.registered

    def register(self, api: Any) -> None:
        if self.registered:
            raise AwError("sdk_hooks_already_installed")
        api.register_middleware(
            lambda _ctx, _config: QwenPawMiddleware(self.instance), priority=100
        )
        self.registered = True
        api.register_startup_hook("aw-sdk-connect", self.startup)
        api.register_shutdown_hook("aw-sdk-close", self.instance.close)

    async def startup(self) -> None:
        if self.automatic:
            await self.instance.connect()
        self.instance.listen(automatic=self.automatic)
