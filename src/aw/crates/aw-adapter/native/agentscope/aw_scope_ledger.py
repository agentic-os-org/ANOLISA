"""Task scope over the AW invocation ledger for one bridge process."""

from __future__ import annotations

import asyncio
import atexit
import json
import os
import signal
from typing import Any


class CallbackFailure(RuntimeError):
    """A fixed adapter failure code, separate from a native policy denial."""


# One task scope per bridge process, opened before the first hook event and
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


def _drop_closed_scopes() -> None:
    """Forget scopes that cannot hold a child binding."""
    for scope in [s for s in _unclosed_scopes if s.closed or s.handle is None]:
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
        if scope.handle is None or scope.closed:
            _drop_closed_scopes()
            continue
        remaining = deadline - asyncio.get_running_loop().time()
        if remaining <= 0:
            break
        await scope._close_binding(remaining)
    _drop_closed_scopes()
    if any(
        scope.pid == pid and scope.handle is not None and not scope.closed
        for scope in _unclosed_scopes
    ):
        raise CallbackFailure("aw_scope_cleanup_failed")


def _exit_cleanup() -> None:
    """Close every scope that may still hold a child binding."""
    for scope in list(_unclosed_scopes) + ([_task_scope] if _task_scope else []):
        if (
            scope is None
            or scope.pid != os.getpid()
            or scope.closed
            or scope.handle is None
        ):
            continue
        try:
            asyncio.run(scope.close())
        except Exception:
            # The service reclaims the binding after the worker exits; a
            # failed best-effort close must not crash interpreter teardown.
            pass


async def _ensure_task_scope(config: dict[str, Any]) -> "AwTaskScope":
    """Open the process scope once; forks open their own on first use."""
    global _task_scope, _task_scope_pid, _task_scope_lock, _task_scope_lock_pid, _atexit_registered
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


class AwTaskScope:
    """Task scope over the AW invocation ledger for one bridge process."""

    def __init__(self, config: dict[str, Any]) -> None:
        self.config = config
        self.pid = os.getpid()
        self.handle: dict[str, Any] | None = None
        self.closed = False
        self.failed = False
        self.sequence = 0
        self.invocations: dict[str, int] = {}
        self.invocation_limit = 0
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

    async def open(self) -> None:
        """Open, acknowledge and enable before the first hook event."""
        if self.handle is not None or self.closed or self.failed:
            return
        # Providers spawned by this scope inherit the admitted launch
        # environment (without bridge import paths), matching root-binding
        # Provider semantics.
        environment = self.config.get("provider_environment")
        if not isinstance(environment, dict):
            environment = dict(os.environ)
        code, stdout, stderr = await self._command(
            "open",
            {"pid": os.getpid(), "cwd": os.getcwd(), "environment": environment},
            33,
        )
        if code != 0:
            raise CallbackFailure(
                "aw_scope_open_failed: " + stderr.decode(errors="replace")[-300:]
            )
        self.handle = json.loads(stdout)
        try:
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
            return
        if code == 0:
            self.closed = True
            # A confirmed close releases the pending-cleanup registration and
            # the last references to this scope's local records.
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
        return await self._command(
            "invoke",
            {"scope": self.handle, "native_input": json.dumps(payload)},
            timeout,
            hook,
            close_event,
        )

    async def close(self) -> None:
        """Retire outstanding invocations and close the scope best-effort."""
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
