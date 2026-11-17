"""Bind the existing resident guard at the first tool using SDK-owned RPCs."""

import asyncio
import os
from copy import deepcopy

from aw_sdk import Admission, AwError
from .aw_resident import ResidentMiddleware


class DeferredScope:
    """Keep local reply ownership until a first tool fixes the immutable policy."""

    def __init__(self, owner, plugin):
        self.owner, self.plugin = owner, plugin
        self.pid = os.getpid()
        self.task = None
        self.admission = None
        self.closed = False
        self.environment = None
        self.cwd = None
        self.invocation_limit = 0
        self.policy_revision = None
        self.watch = None
        self.preparing = None
        self.probe = None
        self.on_failure = None

    def check(self, *, cleanup=False):
        if self.pid != os.getpid() or self.closed or self.environment is None:
            raise AwError("after_scope_unavailable")
        if not cleanup:
            if self.watch is not None:
                self.watch.check()
            if self.probe is not None:
                self.probe()

    async def open(self, cwd, environment):
        if self.environment is not None or self.closed:
            raise AwError("after_scope_reopened")
        self.cwd = os.path.realpath(cwd)
        self.environment = dict(environment)
        self.admission = Admission(
            self.plugin.instance,
            events=list(self.plugin.instance.capabilities["events"]),
            timeout_ms=self.plugin.attach_timeout_ms,
            cwd=self.cwd,
            environment=self.environment,
        )
        if self.plugin.input_enabled:
            await self.ensure()

    async def ensure(self):
        if self.preparing is None:
            self.preparing = asyncio.create_task(self.prepare())
        await asyncio.shield(self.preparing)
        self.check()

    async def prepare(self):
        async with asyncio.timeout(self.plugin.attach_timeout_ms / 1000):
            await self.admission.prepare()
            self.check()
            self.task = self.admission.task
            self.plugin.ready(self.task.routes)
            self.owner.config = {
                **self.owner.root_config,
                "hooks": deepcopy(self.task.routes["hooks"]),
                "policy_revision": self.task.routes["policy_revision"],
            }
            activation = await self.call("enable_invocations")
            self.invocation_limit = activation["invocations_per_binding"]
            self.watch = await self.task.watch_scope(probe=self.probe, on_failure=self.on_failure)
            self.policy_revision = self.watch.policy_revision

    async def call(self, method, **fields):
        if self.task is None:
            raise AwError("after_scope_not_admitted")
        return await self.task.call(
            {
                "method": method,
                "instance_id": self.task.binding["instance_id"],
                **fields,
            }
        )

    async def validate(self):
        self.check()
        if self.task is None:
            raise AwError("after_scope_not_admitted")
        await self.watch.validate()

    async def register(self, sequence, call_id, name, arguments):
        await self.validate()
        await self.call(
            "register_invocation",
            sequence=sequence,
            call_id=call_id,
            tool_name=name,
            input=arguments,
        )

    async def finish(self, sequence, call_id):
        self.check(cleanup=True)
        await self.call("finish_invocation", sequence=sequence, call_id=call_id)

    async def invoke_native(self, hook, payload):
        await self.validate()
        return await self.task.invoke_native(hook, payload)

    async def close(self):
        if self.closed:
            return
        if self.preparing is not None:
            if not self.preparing.done():
                self.preparing.cancel()
            await asyncio.gather(self.preparing, return_exceptions=True)
        if self.admission is not None:
            await self.admission.close()
        self.closed = True
        if self.environment is not None:
            self.environment.clear()


class DeferredResident(ResidentMiddleware):
    """Reuse native drain, stream withholding and checked event publication."""

    def create_scope(self, config):
        return DeferredScope(self, config["deferred_plugin"])

    async def on_acting(self, agent, input_kwargs, next_handler):
        await self.scope.ensure()
        stream = super().on_acting(agent, input_kwargs, next_handler)
        try:
            async for item in stream:
                yield item
        finally:
            await stream.aclose()
