"""Background job lifecycle framework for the daemon."""

import asyncio
import contextlib
import logging
import math
import uuid
from abc import ABC, abstractmethod
from collections.abc import Iterator
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from typing import Any

from agent_sec_cli.correlation_context import (
    TraceContext,
    reset_current_trace_context,
    set_current_trace_context,
)
from agent_sec_cli.daemon.logging import log_daemon_event


@dataclass(frozen=True)
class JobStatus:
    """Serializable background job state."""

    name: str
    state: str
    last_error: str | None = None
    last_tick_at: str | None = None
    interval_seconds: float | None = None
    last_started_at: str | None = None
    next_run_at: str | None = None

    def to_dict(self) -> dict[str, Any]:
        """Return a JSON-serializable status payload."""
        payload: dict[str, Any] = {
            "name": self.name,
            "state": self.state,
            "last_error": self.last_error,
            "last_tick_at": self.last_tick_at,
        }
        if self.interval_seconds is not None:
            payload["interval_seconds"] = self.interval_seconds
        if self.last_started_at is not None:
            payload["last_started_at"] = self.last_started_at
        if self.next_run_at is not None:
            payload["next_run_at"] = self.next_run_at
        return payload


class BackgroundJob(ABC):
    """Base class for daemon background jobs."""

    name = "background-job"

    @abstractmethod
    async def start(self) -> None:
        """Start the job."""
        pass

    @abstractmethod
    async def stop(self) -> None:
        """Stop the job."""
        pass

    @abstractmethod
    def status(self) -> JobStatus:
        """Return current job status."""
        pass


@contextlib.contextmanager
def job_trace_context(job_name: str) -> Iterator[TraceContext]:
    """Set a daemon-owned trace context for one background job run."""
    trace_context = TraceContext(trace_id=str(uuid.uuid4()))
    token = set_current_trace_context(trace_context)
    try:
        yield trace_context
    finally:
        reset_current_trace_context(token)


def _log_job_event(
    *,
    event: str,
    message: str,
    job_name: str,
    job_kind: str,
    state: str,
    trace_context: TraceContext,
    level: int = logging.INFO,
    latency_ms: int | None = None,
    error: Exception | None = None,
    interval_seconds: float | None = None,
) -> None:
    fields: dict[str, Any] = {
        "job_name": job_name,
        "job_kind": job_kind,
        "state": state,
    }
    if latency_ms is not None:
        fields["latency_ms"] = latency_ms
    if interval_seconds is not None:
        fields["interval_seconds"] = interval_seconds
    if error is not None:
        fields["error_type"] = type(error).__name__
        fields["error_message"] = str(error)

    log_daemon_event(
        level=level,
        event=event,
        message=message,
        data=fields,
        trace_context=trace_context,
    )


def _elapsed_ms(started_monotonic: float) -> int:
    return int((time_monotonic() - started_monotonic) * 1000)


class OneShotBackgroundJob(BackgroundJob, ABC):
    """Background job that runs once after startup."""

    def __init__(self) -> None:
        self._task: asyncio.Task[None] | None = None
        self._state = "stopped"
        self._last_error: str | None = None
        self._last_tick_at: str | None = None
        self._last_started_at: str | None = None

    async def start(self) -> None:
        """Start the one-shot job without blocking daemon startup."""
        if self._task is not None and not self._task.done():
            return

        self._state = "running"
        self._task = asyncio.create_task(self._run_once_with_lifecycle())

    async def stop(self) -> None:
        """Cancel the job task if it has not completed."""
        if self._task is not None and not self._task.done():
            self._task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._task

        if self._state == "running":
            self._state = "stopped"
        self._task = None

    def status(self) -> JobStatus:
        """Return current one-shot job status."""
        return JobStatus(
            name=self.name,
            state=self._state,
            last_error=self._last_error,
            last_tick_at=self._last_tick_at,
            last_started_at=self._last_started_at,
        )

    @abstractmethod
    async def run_once(self) -> None:
        """Run the one-shot job body."""
        pass

    def on_run_started(self, started_at: str) -> None:
        """Handle one-shot job start."""
        pass

    def on_run_cancelled(self, finished_at: str) -> None:
        """Handle one-shot job cancellation."""
        pass

    def on_run_failed(self, exc: Exception, finished_at: str) -> None:
        """Handle one-shot job failure."""
        pass

    def on_run_completed(self, finished_at: str) -> None:
        """Handle one-shot job completion."""
        pass

    async def _run_once_with_lifecycle(self) -> None:
        with job_trace_context(self.name) as trace_context:
            started_monotonic = time_monotonic()
            started_at = utc_now()
            self._state = "running"
            self._last_started_at = started_at
            self._last_tick_at = started_at
            _log_job_event(
                event="daemon_job_started",
                message="daemon background job started",
                job_name=self.name,
                job_kind="one_shot",
                state=self._state,
                trace_context=trace_context,
            )

            try:
                self.on_run_started(started_at)
                await self.run_once()
            except asyncio.CancelledError:
                finished_at = utc_now()
                self._last_error = None
                self._state = "stopped"
                try:
                    self.on_run_cancelled(finished_at)
                finally:
                    _log_job_event(
                        event="daemon_job_cancelled",
                        message="daemon background job cancelled",
                        job_name=self.name,
                        job_kind="one_shot",
                        state=self._state,
                        trace_context=trace_context,
                        latency_ms=_elapsed_ms(started_monotonic),
                    )
                raise
            except Exception as exc:
                finished_at = utc_now()
                self._last_error = str(exc)
                self._state = "error"
                try:
                    self.on_run_failed(exc, finished_at)
                finally:
                    _log_job_event(
                        level=logging.ERROR,
                        event="daemon_job_failed",
                        message="daemon background job failed",
                        job_name=self.name,
                        job_kind="one_shot",
                        state=self._state,
                        trace_context=trace_context,
                        latency_ms=_elapsed_ms(started_monotonic),
                        error=exc,
                    )
                return

            finished_at = utc_now()
            self._last_error = None
            self._state = "completed"
            self.on_run_completed(finished_at)
            _log_job_event(
                event="daemon_job_completed",
                message="daemon background job completed",
                job_name=self.name,
                job_kind="one_shot",
                state=self._state,
                trace_context=trace_context,
                latency_ms=_elapsed_ms(started_monotonic),
            )


class PeriodicBackgroundJob(BackgroundJob, ABC):
    """Background job that runs once per interval boundary.

    Scheduling is anchored to each run start time. If a run takes longer than
    one interval, the scheduler skips missed boundaries and waits for the next
    future interval boundary instead of running back-to-back.
    """

    def __init__(self, interval_seconds: float) -> None:
        if interval_seconds <= 0:
            raise ValueError("interval_seconds must be positive")

        self.interval_seconds = interval_seconds
        self._task: asyncio.Task[None] | None = None
        self._stop_event: asyncio.Event | None = None
        self._state = "stopped"
        self._last_error: str | None = None
        self._last_tick_at: str | None = None
        self._last_started_at: str | None = None
        self._next_run_at: str | None = None

    async def start(self) -> None:
        """Start the periodic loop."""
        if self._task is not None and not self._task.done():
            return

        self._stop_event = asyncio.Event()
        self._state = "running"
        self._task = asyncio.create_task(self._run_loop())

    async def stop(self) -> None:
        """Stop the periodic loop."""
        if self._stop_event is not None:
            self._stop_event.set()

        if self._task is not None:
            self._task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._task
            self._task = None

        self._state = "stopped"
        self._stop_event = None

    def status(self) -> JobStatus:
        """Return current periodic job status."""
        return JobStatus(
            name=self.name,
            state=self._state,
            last_error=self._last_error,
            last_tick_at=self._last_tick_at,
            interval_seconds=self.interval_seconds,
            last_started_at=self._last_started_at,
            next_run_at=self._next_run_at,
        )

    @abstractmethod
    async def run_once(self) -> None:
        """Run one periodic job iteration."""
        pass

    async def _run_loop(self) -> None:
        next_run_monotonic = time_monotonic()
        self._next_run_at = utc_now()

        while self._stop_event is not None and not self._stop_event.is_set():
            await self._wait_until(next_run_monotonic)
            if self._stop_event is None or self._stop_event.is_set():
                break

            started_monotonic = time_monotonic()
            started_at = utc_now()
            self._state = "running"
            self._last_started_at = started_at
            self._last_tick_at = started_at

            with job_trace_context(self.name) as trace_context:
                _log_job_event(
                    event="daemon_job_started",
                    message="daemon background job started",
                    job_name=self.name,
                    job_kind="periodic",
                    state=self._state,
                    trace_context=trace_context,
                    interval_seconds=self.interval_seconds,
                )
                try:
                    await self.run_once()
                    self._last_error = None
                    self._state = "running"
                except asyncio.CancelledError:
                    _log_job_event(
                        event="daemon_job_cancelled",
                        message="daemon background job cancelled",
                        job_name=self.name,
                        job_kind="periodic",
                        state="stopped",
                        trace_context=trace_context,
                        latency_ms=_elapsed_ms(started_monotonic),
                        interval_seconds=self.interval_seconds,
                    )
                    raise
                except Exception as exc:
                    self._last_error = str(exc)
                    self._state = "error"
                    _log_job_event(
                        level=logging.ERROR,
                        event="daemon_job_failed",
                        message="daemon background job failed",
                        job_name=self.name,
                        job_kind="periodic",
                        state=self._state,
                        trace_context=trace_context,
                        latency_ms=_elapsed_ms(started_monotonic),
                        error=exc,
                        interval_seconds=self.interval_seconds,
                    )
                else:
                    _log_job_event(
                        event="daemon_job_completed",
                        message="daemon background job completed",
                        job_name=self.name,
                        job_kind="periodic",
                        state=self._state,
                        trace_context=trace_context,
                        latency_ms=_elapsed_ms(started_monotonic),
                        interval_seconds=self.interval_seconds,
                    )

            finished_monotonic = time_monotonic()
            next_run_monotonic = next_cycle_start(
                started_monotonic,
                finished_monotonic,
                self.interval_seconds,
            )
            wait_seconds = max(0.0, next_run_monotonic - finished_monotonic)
            self._next_run_at = utc_after(wait_seconds)

    async def _wait_until(self, run_at_monotonic: float) -> None:
        wait_seconds = max(0.0, run_at_monotonic - time_monotonic())
        if wait_seconds == 0:
            return
        if self._stop_event is None:
            return

        try:
            await asyncio.wait_for(self._stop_event.wait(), timeout=wait_seconds)
        except asyncio.TimeoutError:
            pass


class JobManager:
    """Tracks daemon background jobs and exposes their status."""

    def __init__(self) -> None:
        self._jobs: list[BackgroundJob] = []
        self._started = False

    def register(self, job: BackgroundJob) -> None:
        """Register a background job before daemon startup."""
        self._jobs.append(job)

    def get(self, name: str) -> BackgroundJob | None:
        """Return a registered job by stable name."""
        for job in self._jobs:
            if job.name == name:
                return job
        return None

    async def start_all(self) -> None:
        """Start all registered jobs."""
        for job in self._jobs:
            await job.start()
        self._started = True

    async def stop_all(self) -> None:
        """Stop all registered jobs in reverse registration order."""
        for job in reversed(self._jobs):
            await job.stop()
        self._started = False

    def status(self) -> list[dict[str, Any]]:
        """Return JSON-serializable status for all jobs."""
        return [job.status().to_dict() for job in self._jobs]

    @property
    def started(self) -> bool:
        """Return whether the manager has started its jobs."""
        return self._started


def next_cycle_start(
    started_monotonic: float,
    finished_monotonic: float,
    interval_seconds: float,
) -> float:
    """Return the next interval boundary anchored to a run start time."""
    if interval_seconds <= 0:
        raise ValueError("interval_seconds must be positive")

    elapsed = max(0.0, finished_monotonic - started_monotonic)
    cycle_index = max(1, math.ceil(elapsed / interval_seconds))
    return started_monotonic + (cycle_index * interval_seconds)


def time_monotonic() -> float:
    """Return monotonic time for periodic scheduling."""
    return asyncio.get_running_loop().time()


def utc_now() -> str:
    """Return the current UTC timestamp for job status."""
    return _format_utc(datetime.now(timezone.utc))


def utc_after(seconds: float) -> str:
    """Return a UTC timestamp approximately seconds in the future."""
    return _format_utc(datetime.now(timezone.utc) + timedelta(seconds=seconds))


def _format_utc(value: datetime) -> str:
    return value.isoformat().replace("+00:00", "Z")
