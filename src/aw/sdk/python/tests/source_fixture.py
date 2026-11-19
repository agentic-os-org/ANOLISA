"""Source sessions using a controllable in-memory Task."""

from aw_sdk.sources import LanguageSource


class FakeTask:
    def __init__(self, handler, *, fallback: bool = False) -> None:
        self._handler = handler
        self.fallback = fallback
        self.closed = False
        self.routes = {"events": {}, "hooks": []}
        self.events: list[tuple[str, dict]] = []

    async def emit(self, event: str, payload: dict) -> dict[str, str]:
        self.events.append((event, payload))
        return await self._handler(event, payload)

    async def close(self) -> None:
        self.closed = True


def fake_source(handler, *, events=None, availability: str = "required", fallback: bool = False):
    kwargs = {"events": events} if events is not None else {}
    source = LanguageSource.create(
        endpoint="/tmp/nonexistent-aw.sock",
        target="app",
        availability=availability,
        **kwargs,
    )
    source.instance.state = "detached" if fallback else "ready"
    tasks: list[FakeTask] = []

    async def open_task(**_: object) -> FakeTask:
        task = FakeTask(handler, fallback=fallback)
        task.routes["hooks"] = [{"event": event} for event in source.instance.capabilities["events"]]
        tasks.append(task)
        return task

    source.instance.task = open_task  # type: ignore[method-assign]
    return source, tasks
