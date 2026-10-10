"""Real daemon/SQLite recovery with independently surviving HTTP operations.

The mock follows AgentSight coordinator serialization and its distinct handling
of unknown IDs versus persisted Detached records. Gates model HTTP work queued
before the coordinator and replies delayed after a committed operation.
"""

import json
import os
import shutil
import signal
import sqlite3
import subprocess
import threading
import time
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest
from tests.v2.e2e.conftest import start_daemon as start_daemon

_V2 = Path(__file__).resolve().parents[3] / "v2"
_WIRE = _V2 / "fixtures/clients/agentsight/file-deletion"
_BINDINGS = "/api/enforcement/bindings"


def until(read, ready):
    deadline = time.monotonic() + 10
    while True:
        value = read()
        if ready(value):
            return value
        assert time.monotonic() < deadline, f"recovery did not converge: {value}"
        time.sleep(0.02)


class Gate:
    def __init__(self, boundary, repeat=False):
        self.boundary = boundary
        self.repeat = repeat
        self.entered = threading.Event()
        self.release = threading.Event()
        self.target = None

    def wait(self):
        assert self.entered.wait(5), f"HTTP boundary not reached: {self.boundary}"
        return self.target


class Remote:
    def __init__(self):
        self.lifecycle = threading.Lock()
        self.trace_lock = threading.Lock()
        self.records = {}
        self.requests = []
        self.gates = []
        self.errors = []

    def pause(self, boundary, repeat=False):
        gate = Gate(boundary, repeat)
        self.gates.append(gate)
        return gate

    def checkpoint(self, boundary, target=None):
        with self.trace_lock:
            gate = next(
                (
                    item
                    for item in self.gates
                    if item.boundary == boundary
                    and (
                        not item.entered.is_set()
                        or (
                            item.repeat
                            and item.target == target
                            and not item.release.is_set()
                        )
                    )
                ),
                None,
            )
            if gate is not None:
                gate.target = target
                gate.entered.set()
        if gate is not None and not gate.release.wait(15):
            with self.trace_lock:
                self.errors.append(f"unreleased boundary: {boundary}")
        if gate is not None:
            self.trace(f"RELEASED:{boundary}", target)

    def trace(self, method, target):
        with self.trace_lock:
            self.requests.append((method, target))
            print(
                f"REMOTE {time.monotonic():.6f} thread={threading.get_ident()} {method} {target}",
                flush=True,
            )

    def present(self):
        with self.lifecycle:
            return {
                key
                for key, record in self.records.items()
                if record["state"] == "enforced"
            }

    def posts(self):
        with self.trace_lock:
            return [key for method, key in self.requests if method == "POST"]


class Recovery:
    def __init__(self, remote, start, directory):
        self.remote = remote
        self.start = start
        self.directory = directory
        self.database = directory / "data/policy-state.db"
        self.daemon = None
        self.processes = []
        self.starts = 0
        self.daemons = []
        self.executable = directory / "crash-target"
        shutil.copy2("/bin/sleep", self.executable)

    def restart(self):
        self.starts += 1
        self.daemon = self.start(name=f"daemon-{self.starts}.sock")
        self.daemons.append(self.daemon)
        print(
            f"DAEMON {time.monotonic():.6f} started pid={self.daemon.process.pid}",
            flush=True,
        )
        return self.daemon

    def crash(self):
        print(
            f"DAEMON {time.monotonic():.6f} killing pid={self.daemon.process.pid} "
            f"previous_returncode={self.daemon.process.poll()}",
            flush=True,
        )
        self.daemon.process.kill()
        assert self.daemon.process.wait(timeout=5) == -signal.SIGKILL

    def spawn(self):
        process = subprocess.Popen([str(self.executable), "60"])
        self.processes.append(process)
        return process

    def policy(self, name="crash-policy"):
        template = json.loads(
            (
                _V2 / "crates/asc-policy-types/tests/fixtures/prepared-binding.json"
            ).read_text()
        )["policy"]["template"]
        path = self.directory / "policy.json"
        path.write_text(json.dumps(template))
        return self.daemon.request(
            "policy", "create", "--name", name, "--file", str(path)
        )

    def scope(self, policy, process=None):
        selector = (
            ("--pid", str(process.pid))
            if process
            else ("--executable", str(self.executable))
        )
        return self.daemon.request(
            "scope",
            "create",
            *selector,
            "--policy-id",
            policy["policyId"],
            "--policy-revision",
            "1",
        )

    def saved(self):
        with closing(sqlite3.connect(f"file:{self.database}?mode=ro", uri=True)) as db:
            scopes = db.execute(
                "SELECT scope_id,phase,pinned_process_json FROM scopes"
            ).fetchall()
            bindings = db.execute(
                "SELECT binding_id,phase,deployments_json,status_version FROM bindings"
            ).fetchall()
        return scopes, bindings

    def ready(self, count):
        return until(
            lambda: self.daemon.request("binding", "list"),
            lambda value: value["total"] == count
            and all(item["status"]["phase"] == "READY" for item in value["items"]),
        )["items"]

    def clean(self):
        until(
            lambda: self.daemon.request("binding", "list"),
            lambda value: value["total"] == 0,
        )
        until(
            lambda: self.daemon.request("scope", "list"),
            lambda value: value["total"] == 0,
        )
        assert self.saved() == ([], [])
        assert not self.remote.present(), "local cleanup left an orphaned remote target"


@pytest.fixture
def recovery(tmp_path, monkeypatch, start_daemon):
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path / "data"))
    remote = Remote()
    health = json.loads((_WIRE / "health.response.json").read_text())

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def reply(self, status, body=None):
            payload = json.dumps(body).encode() if body is not None else b""
            try:
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
            except (BrokenPipeError, ConnectionResetError):
                # SIGKILL disconnects the caller; remote work can still finish.
                pass

        def authorized(self):
            if self.headers.get("Authorization", "").startswith("Bearer "):
                return True
            self.reply(401)
            return False

        def do_GET(self):
            if self.authorized():
                remote.checkpoint("health")
                self.reply(
                    200 if self.path == "/api/enforcement/health" else 404, health
                )

        def do_POST(self):
            if not self.authorized():
                return
            assert self.path == _BINDINGS, self.path
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            key = body["binding_id"]
            remote.trace("POST", key)
            remote.checkpoint("before_apply", key)
            with remote.lifecycle:
                previous = remote.records.get(key)
                if previous is not None and previous["request"] != body:
                    self.reply(
                        409, {"error": {"code": "binding_conflict", "retryable": False}}
                    )
                    return
                if previous is None or previous["state"] != "detached":
                    remote.records[key] = {
                        "request": body,
                        "state": "enforced",
                        "domain_id": 41,
                    }
                remote.trace("APPLIED", key)
                response = dict(remote.records[key])
            remote.checkpoint("after_apply", key)
            self.reply(200, response)
            remote.trace("POST_FINISHED", key)

        def do_DELETE(self):
            if not self.authorized():
                return
            assert self.path.startswith(f"{_BINDINGS}/"), self.path
            key = self.path.rsplit("/", 1)[-1]
            remote.trace("DELETE_RECEIVED", key)
            with remote.lifecycle:
                record = remote.records.get(key)
                if record is None:
                    remote.trace("DELETE_404", key)
                else:
                    record["state"] = "detached"
                    record.pop("domain_id", None)
                    remote.trace("DELETE_204", key)
            # The coordinator has returned; a delayed HTTP reply must not hold its lock.
            if record is None:
                self.reply(
                    404, {"error": {"code": "binding_not_found", "retryable": False}}
                )
            else:
                remote.checkpoint("after_delete", key)
                self.reply(204)
            remote.trace("DELETE_REPLIED", key)

    token = Path("/var/log/sysak/.agentsight/.dashboard_token")
    created_token = False
    with ThreadingHTTPServer(("127.0.0.1", 7396), Handler) as server:
        token.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        try:
            descriptor = os.open(token, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        except FileExistsError:
            pass
        else:
            created_token = True
            with os.fdopen(descriptor, "w") as stream:
                stream.write("crash-consistency-test")
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()

        def start_logged(**kwargs):
            # Enable daemon diagnostics without changing the CLI's stderr contract.
            with monkeypatch.context() as environment:
                environment.setenv(
                    "RUST_LOG", os.environ.get("ASC_CRASH_DAEMON_LOG", "warn")
                )
                return start_daemon(**kwargs)

        harness = Recovery(remote, start_logged, tmp_path)
        try:
            yield harness
        finally:
            for gate in remote.gates:
                gate.release.set()
            for index, daemon in enumerate(harness.daemons, start=1):
                process = daemon.process
                if process.poll() is None:
                    process.terminate()
                try:
                    stdout, stderr = process.communicate(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    stdout, stderr = process.communicate(timeout=5)
                (tmp_path / f"daemon-{index}.stderr.log").write_text(stderr or "")
                (tmp_path / f"daemon-{index}.stdout.log").write_text(stdout or "")
                print(
                    f"DAEMON pid={process.pid} rc={process.returncode} stderr={stderr}",
                    flush=True,
                )
            for process in harness.processes:
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=5)
            server.shutdown()
            worker.join(timeout=5)
            if created_token:
                token.unlink()
            assert not remote.errors, remote.errors
