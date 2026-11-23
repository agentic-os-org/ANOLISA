"""AW invocation ledger scopes for replies and the legacy process lifetime."""

from __future__ import annotations

import asyncio
import atexit
import json
import os
import signal
from contextvars import ContextVar
from typing import Any
from uuid import uuid4


class CallbackFailure(RuntimeError):
    """A fixed adapter failure code, separate from a native policy denial."""


# Legacy task scope per bridge process, opened before the first hook event and
# closed at process exit; the invocation ledger recycles correlation capacity
# per call instead of per instance release.
_task_scope: "AwTaskScope | None" = None
_task_scope_pid: int | None = None
_task_scope_lock: asyncio.Lock | None = None
_task_scope_lock_pid: int | None = None
# Scopes whose close was not confirmed yet; they keep their child binding
# until a bounded retry or process-exit cleanup confirms the close.
_unclosed_scopes: "set[AwTaskScope]" = set()
_atexit_registered = False
turn_scope: ContextVar["AwTaskScope | None"] = ContextVar("aw_agentscope_turn_scope", default=None)


def _drop_closed_scopes() -> None:
    """Forget scopes that cannot hold a child binding."""
    for scope in [s for s in _unclosed_scopes if (s.closed or s.handle is None) and s.reservation is None]:
        _unclosed_scopes.discard(scope)
        scope.invocations.clear()


def _active_scope() -> "AwTaskScope | None":
    """Return the process-wide scope when it is usable in this process."""
    global _task_scope, _task_scope_pid
    if _task_scope is None or _task_scope_pid != os.getpid():
        return None
    if _task_scope.closed or _task_scope.failed:
        # An abandoned scope is unusable: publication is dropped so the next
        # tool call opens a fresh one instead of failing every registration.
        if not _task_scope.closed:
            _unclosed_scopes.add(_task_scope)
        _task_scope = None
        return None
    return _task_scope


async def _retry_unclosed_scopes() -> None:
    """Retry pending closes within one budget before allocating a binding."""
    deadline = asyncio.get_running_loop().time() + 6
    pid = os.getpid()
    for scope in list(_unclosed_scopes):
        if scope.pid != pid:
            _unclosed_scopes.discard(scope)
            continue
        if (scope.handle is None or scope.closed) and scope.reservation is None:
            _drop_closed_scopes()
            continue
        if any(not task.done() for task in scope.tasks):
            continue
        remaining = deadline - asyncio.get_running_loop().time()
        if remaining <= 0:
            break
        if scope.reservation is not None:
            try:
                async with asyncio.timeout(remaining):
                    await close_turn_scope(scope)
            except TimeoutError:
                break
        else:
            await scope._close_binding(remaining)
    _drop_closed_scopes()
    if any(
        scope.pid == pid and ((scope.handle is not None and not scope.closed)
                             or scope.reservation is not None)
        for scope in _unclosed_scopes
    ):
        raise CallbackFailure("aw_scope_cleanup_failed")


def _exit_cleanup() -> None:
    """Close every scope that may still hold a child binding."""
    for scope in list(_unclosed_scopes) + ([_task_scope] if _task_scope else []):
        if (
            scope is None
            or scope.pid != os.getpid()
            or ((scope.closed or scope.handle is None) and scope.reservation is None)
        ):
            continue
        try:
            asyncio.run(close_turn_scope(scope))
        except Exception:
            # The service reclaims the binding after the worker exits; a
            # failed best-effort close must not crash interpreter teardown.
            pass


async def _ensure_task_scope(config: dict[str, Any]) -> "AwTaskScope":
    """Reuse the current reply scope or lazily open the legacy process scope."""
    global _task_scope, _task_scope_pid, _task_scope_lock, _task_scope_lock_pid, _atexit_registered
    scope = turn_scope.get()
    if scope is not None:
        scope._check()
        return scope
    scope = _active_scope()
    if scope is not None:
        return scope
    if _task_scope_lock is None or _task_scope_lock_pid != os.getpid():
        _task_scope_lock = asyncio.Lock()
        _task_scope_lock_pid = os.getpid()
    async with _task_scope_lock:
        # Concurrent first tool calls must share one open; the lock keeps the
        # second caller from registering against a superseded child scope.
        scope = _active_scope()
        if scope is not None:
            return scope
        # Retry pending closes first: an unconfirmed close still holds a
        # child binding, and skipping the retry would spend the binding
        # budget on every recovery.
        await _retry_unclosed_scopes()
        scope = AwTaskScope(config)
        if not _atexit_registered:
            atexit.register(_exit_cleanup)
            _atexit_registered = True
        try:
            await scope.open()
        except BaseException:
            if scope.handle is not None and not scope.closed:
                _unclosed_scopes.add(scope)
            raise
        _task_scope = scope
        _task_scope_pid = os.getpid()
    return scope


async def wait_owned(task: asyncio.Task) -> asyncio.CancelledError | None:
    """Remember cancellation while a bounded owned delivery finishes."""
    interrupted = None
    while not task.done():
        try:
            await asyncio.shield(task)
        except asyncio.CancelledError as error:
            interrupted = error
        except Exception:
            # The owner retrieves and handles the operation's original error.
            break
    return interrupted


async def close_turn_scope(scope: "AwTaskScope") -> None:
    """Join the same owned cleanup across cancellation and concurrent retries."""
    task = scope.cleanup_task
    if task is None or task.done():
        task = asyncio.create_task(_close_turn_scope(scope))
        task.add_done_callback(lambda done: None if done.cancelled() else done.exception())
        scope.cleanup_task = task
    await asyncio.shield(task)


async def _close_turn_scope(scope: "AwTaskScope") -> None:
    """Release root occupancy only after native drain and confirmed revocation."""
    try:
        await scope.close()
        if scope.closed and not any(not task.done() for task in scope.tasks):
            if scope.execution is not None:
                await scope.execution.revoke()
            await scope.release()
    finally:
        if scope.reservation is not None:
            _unclosed_scopes.add(scope)


async def open_turn_scope(config: dict[str, Any], execution_factory=None) -> "AwTaskScope":
    """Allocate a reply scope through the existing Service lifecycle."""
    global _atexit_registered
    await _retry_unclosed_scopes()
    scope = AwTaskScope(config)
    if execution_factory is not None:
        from aw_execution import AwExecution

        scope.execution = AwExecution(execution_factory)
    if not _atexit_registered:
        atexit.register(_exit_cleanup)
        _atexit_registered = True
    try:
        # An open may commit before its subprocess response is collected.
        # Settle it and retain the handle before honouring cancellation.
        opening = asyncio.create_task(scope.open())
        interrupted = await wait_owned(opening)
        if interrupted is not None:
            if not opening.cancelled():
                opening.exception()
            cleanup = asyncio.create_task(close_turn_scope(scope))
            await wait_owned(cleanup)
            cleanup.result()
            raise interrupted
        opening.result()
    except BaseException:
        try:
            if scope.reservation is not None or scope.execution is not None:
                cleanup = asyncio.create_task(close_turn_scope(scope))
                await wait_owned(cleanup)
                cleanup.result()
        finally:
            if (scope.handle is not None and not scope.closed) or scope.reservation is not None:
                _unclosed_scopes.add(scope)
        raise
    scope.owner_task = asyncio.current_task()
    return scope


class AwTaskScope:
    """Own an AW child binding and its tool invocation ledger."""

    def __init__(self, config: dict[str, Any]) -> None:
        self.config = config
        self.execution = None
        self.cleanup_task: asyncio.Task | None = None
        self.pid = os.getpid()
        self.handle: dict[str, Any] | None = None
        self.closed = False
        self.failed = False
        self.sequence = 0
        self.reservation: str | None = None
        self.opening_unknown = False
        self.invocations: dict[str, int] = {}
        self.invocation_limit = 0
        self.owner_task: asyncio.Task | None = None
        self.tasks: set[asyncio.Task] = set()
        # Serializes ledger deliveries: the server accepts only the strict
        # next sequence, so concurrent register/finish subprocesses must not
        # reorder or interleave their sequence bookkeeping.
        self.lock = asyncio.Lock()

    def _check(self) -> None:
        if self.handle is None or self.closed or self.failed or self.pid != os.getpid():
            raise CallbackFailure("aw_scope_unavailable")

    async def _command(
        self,
        action: str,
        value: dict[str, Any],
        timeout: float,
        hook: dict[str, Any] | None = None,
        close_event: bool = False,
    ) -> tuple[int, bytes, bytes]:
        arguments = [
            self.config["executable"],
            "scope",
            "--adapter",
            "agentscope",
            "--binding",
            self.config["binding"],
            "--action",
            action,
        ]
        if hook is not None:
            arguments += ["--event", hook["event"], "--step", hook["step"],
                          "--on-error", hook["on_error"]]
        if close_event:
            arguments += ["--close-event", "true"]
        deadline = asyncio.get_running_loop().time() + timeout
        proc = None
        try:
            async with asyncio.timeout_at(deadline):
                proc = await asyncio.create_subprocess_exec(
                    *arguments,
                    stdin=asyncio.subprocess.PIPE,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.PIPE,
                    start_new_session=True,
                    env=self.config.get("provider_environment"),
                )
                stdout, stderr = await proc.communicate(
                    json.dumps(value).encode()
                )
        except BaseException:
            if proc is not None:
                if proc.returncode is None:
                    try:
                        os.killpg(proc.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                await proc.wait()
            raise
        return proc.returncode, stdout, stderr

    async def reserve(self) -> None:
        if not self.config.get("resident_policy_reload"):
            return
        if self.reservation is not None or self.closed or self.pid != os.getpid():
            raise CallbackFailure("aw_task_reservation_unavailable")
        self.reservation = uuid4().hex
        code, _, _ = await self._command(
            "reserve", {"pid": self.pid, "key": self.reservation}, 6,
        )
        if code:
            raise CallbackFailure("aw_task_reservation_failed")

    async def open(self) -> None:
        """Open, acknowledge and enable before the first hook event."""
        if self.handle is not None or self.closed or self.failed:
            return
        if self.config.get("resident_policy_reload") and self.reservation is None:
            await self.reserve()
        if self.execution is not None:
            await self.execution.acquire()
            self.config = {**self.config, "provider_environment": dict(self.execution.lease.hook_environment)}
        # Providers spawned by this scope inherit the admitted launch
        # environment (without bridge import paths), matching root-binding
        # Provider semantics.
        environment = self.config.get("provider_environment")
        if not isinstance(environment, dict):
            environment = dict(os.environ)
        self.opening_unknown = True
        code, stdout, stderr = await self._command(
            "open",
            {"pid": os.getpid(), "cwd": os.getcwd(), "environment": environment},
            33,
        )
        self.opening_unknown = False
        if code != 0:
            raise CallbackFailure(
                "aw_scope_open_failed: " + stderr.decode(errors="replace")[-300:]
            )
        self.handle = json.loads(stdout)
        try:
            if self.reservation is not None:
                code, policy, _ = await self._command("policy", self.handle, 6)
                if code:
                    raise CallbackFailure("aw_task_policy_failed")
                from aw_agentscope import validate_hooks

                hooks = json.loads(policy)["hooks"]
                validate_hooks(hooks)
                self.config = {**self.config, "hooks": hooks}
            code, _, _ = await self._command("acknowledge", self.handle, 6)
            if code != 0:
                self.failed = True
                raise CallbackFailure("aw_scope_acknowledge_failed")
            code, stdout, _ = await self._command("enable", self.handle, 6)
            if code != 0:
                self.failed = True
                raise CallbackFailure("aw_scope_enable_failed")
        except BaseException:
            # Every failure past a successful open owns an acknowledged child
            # binding; close it before propagating so the service reclaims it
            # and the caller can open a fresh scope on its next attempt.
            self.failed = True
            closer = asyncio.create_task(self._command("close", self.handle, 6))
            try:
                await asyncio.shield(closer)
            except asyncio.CancelledError:
                # Repeated cancellation must not abandon the close delivery;
                # wait it out so no background exception goes unretrieved.
                while not closer.done():
                    try:
                        await asyncio.shield(closer)
                    except asyncio.CancelledError:
                        continue
                    except (OSError, TimeoutError, CallbackFailure):
                        break
            except (OSError, TimeoutError, CallbackFailure):
                pass
            finally:
                if not closer.done():
                    closer.cancel()
                try:
                    code, _, _ = await closer
                except (OSError, TimeoutError, CallbackFailure, asyncio.CancelledError):
                    code = None
                if code == 0:
                    self.closed = True
                # An undelivered close keeps atexit cleanup ownership alive.
            raise
        self.invocation_limit = json.loads(stdout)["invocations_per_binding"]

    async def register(self, call_id: str, tool_name: str, tool_input: Any) -> None:
        async with self.lock:
            self._check()
            if len(self.invocations) >= self.invocation_limit:
                # A full ledger is transient: calls already running keep this
                # scope (their after hooks and finishes still work), and later
                # calls reuse the binding once capacity is released. The
                # scope must not be marked failed — that would drop the
                # process owner out from under in-flight calls.
                raise CallbackFailure("aw_invocation_capacity")
            if self.sequence >= 2**53:
                # A permanently exhausted sequence cannot deliver the strict
                # next registration; this scope is genuinely unusable.
                await self._abandon()
                raise CallbackFailure("aw_invocation_capacity")
            # Capture the allocation before the await: a concurrent call in
            # the same scope must not observe an advanced counter for this
            # still-unsent registration.
            sequence = self.sequence + 1
            # The server can commit before the await returns. The delivery runs
            # as a shielded task: a cancellation unblocks the waiter only after
            # the delivery completes and is booked, so the ledger lock keeps
            # registrations ordered and a committed one is never forgotten.
            delivery = asyncio.create_task(
                self._command(
                    "register",
                    {
                        "scope": self.handle,
                        "sequence": sequence,
                        "call_id": call_id,
                        "tool_name": tool_name,
                        "input": tool_input,
                    },
                    6,
                )
            )
            try:
                code, _, _ = await asyncio.shield(delivery)
            except asyncio.CancelledError:
                interrupted = True
                # Repeated cancellation must not abandon the in-flight
                # delivery: the lock stays held until the command settles and
                # the outcome is booked, keeping ledger order intact.
                while not delivery.done():
                    try:
                        await asyncio.shield(delivery)
                    except asyncio.CancelledError:
                        continue
                    except (OSError, TimeoutError):
                        break
                if interrupted:
                    # Book as committed (an unknown outcome counts as
                    # committed) so the drain retires the server invocation.
                    self.sequence = sequence
                    self.invocations[call_id] = sequence
                    raise
            except (OSError, TimeoutError) as error:
                # Transport loss after an unknown outcome: booking keeps the
                # strict sequence aligned with what the server may hold.
                self.sequence = sequence
                self.invocations[call_id] = sequence
                raise CallbackFailure("aw_invocation_register_rejected") from error
            if code is not None and code != 0:
                # The rejected admission still consumes the sequence
                # server-side, so this scope can no longer deliver the
                # strict next sequence; keep it unusable instead of
                # desynchronizing later registrations.
                self.failed = True
                raise CallbackFailure("aw_invocation_register_rejected")
            self.sequence = sequence
            self.invocations[call_id] = sequence

    async def finish(self, call_id: str) -> None:
        """Retire one invocation; keep the local record until the server confirms."""
        async with self.lock:
            sequence = self.invocations.get(call_id)
            if sequence is None:
                raise CallbackFailure("aw_invocation_already_retired")
            try:
                code, _, _ = await self._command(
                    "finish",
                    {"scope": self.handle, "sequence": sequence, "call_id": call_id},
                    6,
                )
            except (OSError, TimeoutError) as error:
                await self._abandon()
                raise CallbackFailure("aw_invocation_finish_failed") from error
            if code != 0:
                await self._abandon()
                raise CallbackFailure("aw_invocation_finish_failed")
            self.invocations.pop(call_id, None)

    async def _abandon(self) -> None:
        """Close the scope for good on an uncertain retirement.

        A transient finish fault cannot be retried against the server's strict
        sequence state, so the scope is closed (the server's close reclaims the
        binding and its remaining invocations), marked unusable, and subsequent
        calls may open a fresh scope after cleanup. This path never re-finishes
        booked calls: finish failures are
        what brought us here, and close() itself would recurse back.
        """
        if self.closed:
            return
        self.failed = True
        if self.handle is None:
            self.closed = True
            return
        await self._close_binding()

    async def _close_binding(self, timeout: float = 6) -> None:
        """Deliver the close RPC once; mark closed only on confirmation.

        An undelivered close keeps cleanup ownership: the atexit handler
        retries it, so a transient failure cannot leak the child binding.
        """
        try:
            code, _, _ = await self._command("close", self.handle, timeout)
        except (OSError, TimeoutError, CallbackFailure):
            # Undelivered: closed stays False so atexit retries the close.
            _unclosed_scopes.add(self)
            return
        if code != 0:
            _unclosed_scopes.add(self)
        if code == 0:
            self.closed = True
            # A confirmed close releases the pending-cleanup registration and
            # the last references to this scope's local records.
            if self.reservation is None:
                _unclosed_scopes.discard(self)
            self.invocations.clear()

    async def invoke(
        self,
        hook: dict[str, Any],
        payload: dict[str, Any],
        timeout: float,
        close_event: bool = False,
    ) -> tuple[int, bytes, bytes]:
        self._check()
        from aw_execution import public_value

        payload = public_value(payload)
        return await self._command(
            "invoke",
            {"scope": self.handle, "native_input": json.dumps(payload)},
            timeout,
            hook,
            close_event,
        )

    async def release(self) -> None:
        if self.reservation is None:
            return
        _unclosed_scopes.add(self)
        if (
            not self.closed or self.pid != os.getpid()
            or any(not task.done() for task in self.tasks)
        ):
            return
        try:
            code, _, _ = await self._command(
                "release", {"pid": self.pid, "key": self.reservation}, 6,
            )
        except (OSError, TimeoutError, CallbackFailure):
            return
        if code == 0:
            self.reservation = None
            _unclosed_scopes.discard(self)

    async def close(self) -> None:
        """Retire native work before releasing its task reservation."""
        # A failed retirement can close the child while concurrent SDK tools
        # still run; drain their cleanup before trusting the closed state.
        tasks = [task for task in self.tasks if not task.done()]
        for task in tasks:
            # A stream owner may already be awaiting asynchronous tool teardown.
            # Repeated cancellation must not interrupt those finally blocks.
            if not task.cancelling():
                task.cancel()
        if tasks:
            _, pending = await asyncio.wait(tasks, timeout=6)
            if pending:
                self.failed = True
                _unclosed_scopes.add(self)
                return
        if self.handle is None:
            self.closed = not self.opening_unknown
        if self.handle is None or self.closed:
            return
        # Exit teardown runs on a fresh event loop after the runtime loop is
        # gone; a lock bound to that dead loop would refuse every finish.
        self.lock = asyncio.Lock()
        try:
            for call_id in list(self.invocations):
                sequence = self.invocations.get(call_id)
                if sequence is None:
                    continue
                try:
                    await self._command(
                        "finish",
                        {"scope": self.handle, "sequence": sequence, "call_id": call_id},
                        6,
                    )
                except (OSError, TimeoutError, CallbackFailure):
                    pass
                self.invocations.pop(call_id, None)
            await self._close_binding()
        except (OSError, TimeoutError, CallbackFailure):
            pass
