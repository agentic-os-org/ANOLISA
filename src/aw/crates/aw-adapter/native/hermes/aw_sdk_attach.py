"""Cooperative SDK control for an application-owned, in-process Hermes Gateway."""

import asyncio
import json
from pathlib import Path
from threading import Event, get_ident

from aw_sdk import Admission, Instance
from aw_sdk.deployment import load_descriptor
from . import aw_result
from .aw_gateway_attach import Turn, install_turns
from .aw_identity import verified_revision


def _callback_timeout(hook):
    return hook["budget_ms"] / 1000 + 5


class SdkAttachment:
    def __init__(self, ctx, verify_hooks, config_file, descriptor_file, target, timeout_ms):
        profile = {"adapter": "hermes", "version": "installed-native-hooks/v1", "entrypoint": "sdk-worker"}
        wiring = load_descriptor(config_file, descriptor_file, profile, target, requires_service=True)
        self.ctx, self.verify_hooks = ctx, verify_hooks
        self.wait_ms = timeout_ms
        self.loop = asyncio.get_running_loop()
        self.thread = get_ident()
        self.failed = Event()
        self.instance = Instance(
            endpoint=wiring["endpoint"], target=wiring["target"], availability="required",
            capabilities={**profile, "events": {"tool.before": ["observe", "block"],
                          "tool.after": ["observe", "replace_result"]},
                          "failure_actions": {"tool.after": ["withhold_result"]}},
            install=self.ready, reconnect_ms=100)

    async def ready(self, routes):
        from hermes_cli import version_info

        # Check the executing tree off the Gateway loop, including unstamped frozen images.
        await asyncio.to_thread(verified_revision, Path(version_info.__file__).resolve().parent.parent)
        return self.callbacks_installed(routes)

    def callbacks_installed(self, routes):
        from hermes_cli.plugins import _resolve_hook_callback_timeout

        self.verify_hooks()
        if not self.turns_installed():
            return False
        from . import aw_gateway
        if any(getattr(module, name) is not installed
               for module, name, _, installed in aw_gateway._restores if installed is not None):
            return False
        timeout = _resolve_hook_callback_timeout()
        for hook in routes["hooks"]:
            if hook["event"] not in self.instance.capabilities["events"]:
                return False
            if hook["event"] == "tool.before" and not (timeout == 0 or _callback_timeout(hook) <= timeout):
                return False
        return True

    def install(self, guards, add_turn, remove_turn, idle):
        guards()
        self.idle = idle
        self.turns_installed = install_turns(lambda *ids: SdkTurn(self, *ids), add_turn, remove_turn)
        self.instance.listen()

    def call(self, async_fn, *args, timeout):
        if get_ident() == self.thread or self.loop.is_closed():
            raise RuntimeError("Hermes tool requires a live worker thread and Gateway loop")
        future = asyncio.run_coroutine_threadsafe(async_fn(*args), self.loop)
        try:
            return future.result(timeout)
        except TimeoutError:
            future.cancel()
            raise

    async def close(self):
        if not self.idle():
            raise RuntimeError("Hermes Gateway still has active turns")
        await self.instance.close()


class SdkTurn(Turn):
    def admit(self, make_binding):
        cwd = self.owner.ctx.session_cwd()
        if not cwd or not Path(cwd).is_absolute():
            raise ValueError("Hermes Gateway requires an absolute session workspace")
        wait_ms = self.owner.wait_ms
        self.scope = Admission(self.owner.instance, events=["tool.before", "tool.after"],
                               timeout_ms=wait_ms, cwd=str(Path(cwd).resolve(strict=True)),
                               install=self.owner.callbacks_installed)
        try:
            self.owner.call(self.scope.prepare, timeout=wait_ms / 1000 + 5)
        except Exception as error:
            raise RuntimeError("AW Hermes first tool could not be admitted") from error
        self.binding = SdkBinding(self.owner, self.scope.task, self.session)

    def finish(self):
        with self.lock:
            self.finished = True
            if self.scope is not None:
                self.owner.call(self.scope.close, timeout=10)


class SdkBinding:
    final_result = True
    after = ()
    replacement = None

    def __init__(self, owner, task, session):
        self.owner, self.task, self.identity = owner, task, session
        self.cwd = task.cwd
        self.before = [(hook, None) for hook in task.routes["hooks"] if hook["event"] == "tool.before"]

    def check(self):
        cwd = self.owner.ctx.session_cwd()
        if not cwd or not Path(cwd).is_absolute() or Path(cwd).resolve(strict=True) != Path(self.cwd):
            raise ValueError("Hermes Gateway session workspace changed")

    def invoke(self, hook, spec, event, values):
        from agent.shell_hooks import _parse_pre_tool_call

        native = {"hook_event_name": event, "cwd": self.cwd, "tool_name": values["tool_name"],
                  "session_id": values["session_id"], "tool_input": values["args"], "extra": values}
        try:
            code, stdout, _ = self.owner.call(self.task.invoke_native, hook, native,
                                              timeout=_callback_timeout(hook))
            try:
                reply = json.loads(stdout or b"{}")
            except (ValueError, RecursionError):
                reply = None
            directive = _parse_pre_tool_call(reply) if isinstance(reply, dict) else None
            if directive and (directive["action"] == "block" or
                              (code == 0 and directive["action"] == "approve")):
                return directive, {}
            if code == 2:
                return {"action": "block", "message": "AW policy blocked the tool"}, {}
            if code or reply != {}:
                raise ValueError("unexpected AW before-tool reply")
        except Exception:
            if hook["on_error"] == "block":
                raise
        return None, {}

    def evaluate(self, payload):
        def invoke(hook):
            code, stdout, _ = self.owner.call(self.task.invoke_native, hook, payload,
                                              timeout=_callback_timeout(hook))
            if code:
                raise ValueError("AW result callback failed")
            return stdout

        return aw_result.select_result(self.task.routes["hooks"], payload, invoke)
