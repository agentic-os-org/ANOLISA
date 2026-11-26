"""Attach an application-owned Gateway and hold its first protected tool."""

import asyncio
from contextvars import ContextVar
import json
import logging
import os
from pathlib import Path
import subprocess
import tempfile
from threading import Event, Lock, Thread
import time

LOG = logging.getLogger(__name__)
_current_turn = ContextVar("aw_hermes_turn", default=None)


class Attachment:
    def __init__(self, config, ctx, load, verify_hooks):
        self.config = config
        self.binary = os.environ.get("AW_HERMES_GATEWAY_BINARY", "/usr/bin/aw")
        self.target = os.environ.get("AW_HERMES_GATEWAY_TARGET", "hermes")
        self.wait_option = os.environ.get("AW_HERMES_GATEWAY_WAIT_MS", "3000")
        self.wait_ms = 3000
        self.ctx, self.load, self.verify_hooks = ctx, load, verify_hooks
        self.settled = Event()
        self.failed = Event()
        self.thread = None
        self.launch = None
        self._lock = Lock()
        self._closing = False
        self._preparations = set()

    def install(self, install_guards, add_turn, remove_turn, idle):
        from gateway import run_runtime

        publish = run_runtime.publish_gateway_runtime_ready
        settle = run_runtime.settle_gateway_runtime

        def ready(runner):
            if runner.config.multiplex_profiles:
                raise RuntimeError("AW Hermes attach requires a dedicated Gateway profile")
            install_guards()
            publish(runner)
            self.thread = Thread(target=self.prepare, name="aw-hermes-attach", daemon=True)
            self.thread.start()

        async def shutdown(runner, budget=None, reason=None):
            await settle(runner, budget, reason)
            await asyncio.to_thread(self.close, idle)

        install_turns(lambda *ids: Turn(self, *ids), add_turn, remove_turn)
        run_runtime.publish_gateway_runtime_ready = ready
        run_runtime.settle_gateway_runtime = shutdown

    def prepare(self):
        started = time.monotonic_ns()
        LOG.info("AW Hermes attach started ns=%d", started)
        try:
            wait_ms = int(self.wait_option)
            if not (Path(self.config).is_absolute() and Path(self.binary).is_absolute()
                    and 100 <= wait_ms <= 60000):
                raise ValueError("invalid AW Hermes Gateway attach options")
            self.wait_ms = wait_ms
            self.launch = self.register(Path.cwd().resolve(), 90)
            LOG.info("AW Hermes attach ready ns=%d", time.monotonic_ns())
        except Exception:
            LOG.exception("AW Hermes Gateway attachment failed; tools remain blocked")
        finally:
            self.settled.set()

    def register(self, cwd, timeout):
        from hermes_cli.plugins import _resolve_hook_callback_timeout
        from .aw_identity import runtime_revision
        from hermes_constants import get_hermes_home

        profile = get_hermes_home().resolve()
        revision = runtime_revision()
        with self._lock:
            if self._closing:
                raise RuntimeError("AW Hermes Gateway is stopping")
            child = subprocess.Popen(
                [self.binary, "instance", "prepare", "--config", self.config, "--agent",
                 self.target, "--native-profile", str(profile), "--gateway-revision", revision,
                 "--gateway-hook-timeout", str(_resolve_hook_callback_timeout())],
                cwd=cwd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            self._preparations.add(child)
        try:
            out, error = child.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.cancel(child)
            raise
        finally:
            with self._lock:
                self._preparations.discard(child)
        if child.returncode:
            raise RuntimeError("AW native registration failed: " +
                               error.decode(errors="replace").strip()[-1024:])
        receipt = json.loads(out)
        binding_path = receipt["binding"]
        try:
            path = receipt["environment"]["AW_HERMES_LAUNCH"]
            launch = self.load(path)
            if (Path(launch["binary"]) != Path(self.binary).resolve() or launch["binding"] != binding_path
                    or Path(launch["cwd"]) != cwd or Path(launch["profile"]) != profile
                    or launch["revision"] != revision or launch["submissions"]):
                raise ValueError("AW Hermes Gateway launch identity differs")
            self.verify_hooks()
            self.lifecycle(binding_path, "ready")
            return path, launch
        except Exception:
            self.release(binding_path)
            raise

    @staticmethod
    def cancel(child):
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=2)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=2)
        child.communicate()

    def lifecycle(self, binding, phase, key=""):
        subprocess.run(
            [self.binary, "bridge", "--adapter", "hermes", "--binding", binding],
            input=json.dumps({"phase": phase, "key": key, "pid": os.getpid()}).encode(),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5, check=True)

    def release(self, binding):
        try:
            subprocess.run([self.binary, "instance", "release", "--binding", binding],
                           stdin=subprocess.DEVNULL, timeout=12, check=True)
        except (OSError, subprocess.SubprocessError):
            LOG.exception("AW Hermes binding cleanup was not confirmed")

    def workspace(self, action, binding, timeout, cwd=None):
        return subprocess.run(
            [self.binary, "bridge", "--adapter", "hermes", "--binding", binding,
             "--gateway-scope", action],
            input=json.dumps(str(cwd)).encode() if cwd is not None else b"",
            stdout=subprocess.PIPE, timeout=timeout, check=True).stdout

    def close(self, idle):
        with self._lock:
            self._closing = True
            for child in self._preparations:
                if child.poll() is None:
                    child.terminate()
        if self.thread is not None:
            self.thread.join(timeout=5)
        if self.settled.is_set() and self.launch is not None and idle():
            self.release(self.launch[1]["binding"])


def install_turns(make_turn, add_turn, remove_turn):
    from gateway import session_finite, session_runtime_workers
    from gateway.session_managed_worker import managed_policy

    execute = session_finite.execute_finite_admission
    start_worker = session_runtime_workers.start_turn_worker

    def track_worker(runner, worker, holder, run_sync):
        start_worker(runner, worker, holder, run_sync)
        turn = _current_turn.get()
        if turn is not None:
            turn.workers.append(worker.worker_done)

    async def protected(authority, ref, row):
        # Keep refusal inside Hermes' native claim and settlement boundary.
        if managed_policy(authority, ref) is not None:
            raise RuntimeError("AW Hermes attach does not support out-of-process turns")
        turn = make_turn(ref.session_id, row.get("request_id") or row["admission_id"],
                         row["admission_id"])
        add_turn(turn)
        token = _current_turn.set(turn)
        try:
            return await execute(authority, ref, row)
        finally:
            _current_turn.reset(token)

            async def finish():
                # Cancelling a Gateway waiter does not stop its executor thread.
                while any(not worker.is_set() for worker in turn.workers):
                    await asyncio.sleep(.05)
                try:
                    await asyncio.to_thread(turn.finish)
                except Exception:
                    turn.owner.failed.set()
                    LOG.exception("AW Hermes tool cleanup was not confirmed")
                remove_turn(turn)

            cleanup = session_runtime_workers.track_mutation(authority, finish())
            try:
                await asyncio.wait_for(asyncio.shield(cleanup), 5)
            except TimeoutError:
                LOG.warning("AW Hermes turn cleanup is still waiting for native completion")

    session_finite.execute_finite_admission = protected
    session_runtime_workers.start_turn_worker = track_worker
    return lambda: (session_finite.execute_finite_admission is protected and
                    session_runtime_workers.start_turn_worker is track_worker)


class Turn:
    def __init__(self, owner, session, request, admission):
        self.owner, self.session, self.request, self.admission = owner, session, request, admission
        self.lock = Lock()
        self.workers = []
        self.binding = None
        self.scope = None
        self.attempted = False
        self.finished = False

    def resolve(self, make_binding):
        with self.lock:
            if self.finished:
                raise RuntimeError("AW Hermes Gateway turn has finished")
            if self.owner.failed.is_set():
                raise RuntimeError("AW Hermes Gateway requires restart after unconfirmed turn cleanup")
            if not self.attempted:
                self.attempted = True
                self.admit(make_binding)
            if self.binding is None:
                raise RuntimeError("AW Hermes attachment is unavailable")
            return self.binding

    def admit(self, make_binding):
        started = time.monotonic_ns()
        deadline = time.monotonic() + self.owner.wait_ms / 1000
        self.owner.settled.wait(self.owner.wait_ms / 1000)
        try:
            if self.owner.launch is None:
                return
            cwd = self.owner.ctx.session_cwd()
            if not cwd or not Path(cwd).is_absolute():
                raise ValueError("Hermes Gateway session has no absolute workspace")
            cwd = Path(cwd).resolve(strict=True)
            path, launch = self.owner.launch
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return
            receipt = self.owner.workspace("open", launch["binding"], remaining, cwd)
            # Keep the Scope private to the plugin and its callback subprocesses.
            self.scope = tempfile.NamedTemporaryFile(
                prefix="workspace-", dir=Path(launch["binding"]).parent)
            self.scope.write(receipt)
            self.scope.flush()
            binding = make_binding(path, launch, self.owner.ctx, self.owner.load,
                                   workspace=cwd, scope=self.scope.name)
            binding.identity = self.session
            binding.request_id, binding.admission_id = self.request, self.admission
            binding.check()
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return
            self.owner.workspace("activate", self.scope.name, remaining)
            self.binding = binding
        except (OSError, ValueError, LookupError, RuntimeError, subprocess.SubprocessError) as error:
            raise RuntimeError("AW Hermes request could not be admitted") from error
        finally:
            LOG.info("AW Hermes first tool wait start_ns=%d end_ns=%d ready=%s",
                     started, time.monotonic_ns(), self.binding is not None)

    def finish(self):
        with self.lock:
            self.finished = True
            if self.scope is not None:
                try:
                    self.owner.workspace("close", self.scope.name, 10)
                finally:
                    self.scope.close()
