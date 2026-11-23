"""One authenticated application loop consumes cooperative control intents."""

import asyncio
import os
from pathlib import Path
import re
from uuid import uuid4
from typing import TYPE_CHECKING

from aw_sdk.transport import AwError, rpc, same_identity

if TYPE_CHECKING:
    from aw_sdk.client import Instance

VERSION = "aw-sdk-control/v1"


class Controller:
    def __init__(self, instance: "Instance", automatic: bool) -> None:
        self.instance = instance
        self.enabled = automatic
        self.stopped = False
        self.finished = False
        self.identity = None
        self.active = None
        self.wake = asyncio.Event()
        self.stopping = None
        self.running = asyncio.create_task(self.loop())

    async def stop(self) -> None:
        if self.stopping is None:
            self.stopping = asyncio.create_task(self.finish())
        try:
            await asyncio.shield(self.stopping)
        finally:
            if self.stopping.done():
                self.finished = True

    async def finish(self) -> None:
        self.stopped = True
        self.wake.set()
        await self.running
        if self.identity and self.instance.control_key:
            try:
                await self.call({"action": "withdraw", "key": self.instance.control_key})
            except AwError as error:
                if not error.unavailable and error.code != "stale_service":
                    raise

    async def loop(self) -> None:
        while not self.stopped:
            try:
                await self.cycle()
            except Exception as error:
                unavailable = isinstance(error, AwError) and error.unavailable
                if unavailable and self.instance.connection:
                    self.instance.lost(self.instance.connection, error)
                if not unavailable and not (
                    isinstance(error, AwError) and error.code == "stale_service"
                ):
                    self.instance.last_error = error
                    self.enabled = False
                    if isinstance(error, AwError) and error.code == "sdk_control_unsupported":
                        self.stopped = True
            if not self.stopped:
                try:
                    await asyncio.wait_for(
                        self.wake.wait(), min(self.instance.reconnect_ms, 1000) / 1000
                    )
                except TimeoutError:
                    pass

    async def call(self, command: dict) -> dict:
        response = await rpc(
            self.instance.endpoint,
            self.identity,
            {"method": "sdk_control", "command": command},
            self.instance.timeout_ms,
        )
        return response["result"]

    async def cycle(self) -> None:
        instance = self.instance
        discovery = await rpc(instance.endpoint, None, {"method": "status"}, instance.timeout_ms)
        if discovery["result"].get("sdk_control_protocol") != VERSION:
            raise AwError("sdk_control_unsupported")
        if not same_identity(self.identity, discovery["identity"]):
            if instance.connection and not same_identity(
                instance.connection["identity"], discovery["identity"]
            ):
                instance.lost(instance.connection)
            self.active = None
            self.identity = discovery["identity"]
        if instance.control_key is None:
            ticks = Path("/proc/self/stat").read_text().rsplit(")", 1)[1].split()[19]
            instance.control_key = {
                "request_id": str(uuid4()),
                "owner": {"pid": os.getpid(), "start_ticks": ticks},
            }
        snapshot = await self.call(
            {
                "action": "poll",
                "key": instance.control_key,
                "target": instance.target,
                "availability": instance.availability,
                "handle": instance.connection["handle"] if instance.connection else None,
            }
        )
        instance.application_id = snapshot["application_id"]
        request = snapshot.get("request")
        if self.stopped:
            return
        if self.active and self.active["request_id"] != (request or {}).get("request_id"):
            if not self.active["was_ready"]:
                await instance.detach_control()
            self.enabled = self.active["was_enabled"]
            self.active = None
        if request and (
            request["state"] == "pending"
            or (
                self.active
                and self.active["request_id"] == request["request_id"]
                and request["state"] == "completed"
            )
        ):
            if self.active is None:
                self.active = {
                    **request,
                    "was_ready": instance.state == "ready",
                    "was_enabled": self.enabled,
                }
            await self.apply(self.active)
        elif self.active:
            if not self.active["was_ready"]:
                await instance.detach_control()
            self.enabled = self.active["was_enabled"]
            self.active = None
        elif self.enabled:
            await instance.connect()

    async def acknowledge(
        self, request: dict, handle: dict | None = None, error: str | None = None
    ) -> dict:
        return await self.call(
            {
                "action": "acknowledge",
                "key": self.instance.control_key,
                "request_id": request["request_id"],
                "handle": handle,
                "error": error,
            }
        )

    async def apply(self, request: dict) -> None:
        instance = self.instance
        try:
            if request["intent"] == "attach":

                async def confirm(connection: dict) -> None:
                    await self.acknowledge(request, connection["handle"])

                instance.confirm_control = confirm
                if instance.state == "ready":
                    await self.acknowledge(request, instance.connection["handle"])
                elif not await instance.connect():
                    return
                self.enabled = True
            else:
                if any(not task.closed and not task.fallback for task in instance.tasks):
                    raise AwError("sdk_control_busy")
                await instance.detach_control()
                await self.acknowledge(request)
                self.enabled = False
            self.active = None
        except Exception as error:
            if isinstance(error, AwError) and (error.unavailable or error.code == "stale_service"):
                return
            if not request["was_ready"]:
                await instance.detach_control()
            code = error.code if isinstance(error, AwError) else "sdk_control_failed"
            if not code.startswith("sdk_control_"):
                instance.last_error = error
            self.enabled = request["was_enabled"]
            try:
                await self.acknowledge(
                    request,
                    error=(
                        code
                        if re.fullmatch(r"[A-Za-z0-9_.:-]{1,128}", code)
                        else "sdk_control_failed"
                    ),
                )
            except AwError as failure:
                if failure.unavailable or failure.code not in {
                    "sdk_control_request_expired",
                    "sdk_control_request_unknown",
                }:
                    raise
            self.active = None
        finally:
            instance.confirm_control = None
