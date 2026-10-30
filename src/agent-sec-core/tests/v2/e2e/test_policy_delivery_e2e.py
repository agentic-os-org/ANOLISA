"""CLI/daemon delivery through HTTP mocks, plus an opt-in real enforcement chain.

Mock tests cover production client/repository behavior, not kernel enforcement.
The live test requires candidate binaries and private mount/network namespaces,
with the initial PID namespace retained; SystemAudit is outside its scope.
"""

import errno
import json
import os
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from collections.abc import Callable, Iterator
from contextlib import closing
from http.client import HTTPConnection, HTTPException
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from queue import Queue
from types import SimpleNamespace
from typing import Any
from urllib.error import URLError

import pytest

from tests.v2.e2e import conftest as e2e
from tests.v2.e2e.conftest import DaemonHandle

_V2 = Path(__file__).resolve().parents[3] / "v2"
_WIRE_FIXTURES = _V2 / "fixtures/clients/agentsight/file-deletion"
_TOKEN_PATH = Path("/var/log/sysak/.agentsight/.dashboard_token")
_BINDINGS_PATH = "/api/enforcement/bindings"
# NS_GET_PARENT also returns EPERM for inaccessible parents, so use Linux's reserved inode.
_INITIAL_PID_NS_INODE = 0xEFFFFFFC


def _fixture(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text())


@pytest.fixture
def agentsight_mock() -> Iterator[Queue]:
    """Capture real requests; never replace or expose existing host credentials."""
    requests: Queue = Queue()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, _format: str, *args: Any) -> None:
            pass

        def _reply(self, status: int, body: dict[str, Any] | None = None) -> None:
            payload = json.dumps(body).encode() if body is not None else b""
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def _authorized(self) -> bool:
            # Observe authentication without reading or recording the token value.
            if self.headers.get("Authorization", "").startswith("Bearer "):
                return True
            self._reply(401)
            return False

        def do_GET(self) -> None:
            requests.put(("GET", self.path, None))
            if self._authorized():
                if self.path == "/api/enforcement/health":
                    health = _fixture(_WIRE_FIXTURES / "health.response.json")
                    health["capabilities"].update(
                        file_entry_delete_protection=True,
                        directory_tree_delete_protection=True,
                        ordinary_write_protection=True,
                        static_mount_topology=True,
                    )
                    self._reply(200, health)
                else:
                    self._reply(404)

        def do_POST(self) -> None:
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.put(("POST", self.path, body))
            if self._authorized():
                if self.path == _BINDINGS_PATH:
                    response = _fixture(_WIRE_FIXTURES / "apply.response.json")
                    response["request"] = body
                    self._reply(200, response)
                else:
                    self._reply(404)

        def do_DELETE(self) -> None:
            requests.put(("DELETE", self.path, None))
            if self._authorized():
                self._reply(204 if self.path.startswith(f"{_BINDINGS_PATH}/") else 404)

    # Bind first: an existing AgentSight must never be used as this test's target.
    with HTTPServer(("127.0.0.1", 7396), Handler) as server:
        _TOKEN_PATH.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        created_token = False
        try:
            try:
                descriptor = os.open(
                    _TOKEN_PATH, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600
                )
            except FileExistsError:
                pass
            else:
                created_token = True
                with os.fdopen(descriptor, "w") as token:
                    token.write("policy-delivery-e2e")
            worker = threading.Thread(target=server.serve_forever, daemon=True)
            worker.start()
            try:
                yield requests
            finally:
                server.shutdown()
                worker.join(timeout=5)
                assert not worker.is_alive(), "AgentSight mock did not stop"
        finally:
            if created_token:
                _TOKEN_PATH.unlink()


def _wait_for(
    read: Callable[[float], dict[str, Any]], ready: Callable[[dict[str, Any]], bool]
) -> dict[str, Any]:
    deadline = time.monotonic() + 30
    actual = None
    while (remaining := deadline - time.monotonic()) > 0:
        actual = read(remaining)
        if ready(actual) and time.monotonic() <= deadline:
            return actual
        time.sleep(min(0.05, max(0.0, deadline - time.monotonic())))
    pytest.fail(f"Policy lifecycle did not converge within 30s: last_state={actual}")


@pytest.mark.parametrize("structured", [False, True], ids=["legacy", "structured"])
def test_discovered_binding_delivers_saved_revision_and_cleans_up(
    tmp_path: Path,
    agentsight_mock: Queue,
    start_daemon: Callable,
    monkeypatch: pytest.MonkeyPatch,
    structured: bool,
) -> None:
    data = tmp_path / "policy-data"
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(data))
    daemon: DaemonHandle = start_daemon()
    template = _fixture(
        _V2 / "crates/asc-policy-types/tests/fixtures/prepared-binding.json"
    )["policy"]["template"]
    if structured:
        template = {
            "specVersion": "0.2",
            "fileProtection": {
                "topology": "static_at_apply",
                "targets": [
                    {
                        "path": "/workspace/protected",
                        "kind": "directory_tree",
                        "protections": {"delete": True, "ordinaryWrite": True},
                        "exclusions": [
                            {
                                "path": "/workspace/protected/cache",
                                "kind": "directory_subtree",
                            }
                        ],
                    }
                ],
            },
        }
    template_file = tmp_path / "template.json"
    template_file.write_text(json.dumps(template))
    original = daemon.request(
        "policy", "create", "--name", "delivery-v1", "--file", str(template_file)
    )
    policy_id = original["policyId"]
    assert original == {
        "policyId": policy_id,
        "policyName": "delivery-v1",
        "revision": 1,
        "template": template,
    }
    assert (
        daemon.request("policy", "get", "--policy-id", policy_id, "--revision", "1")
        == original
    )
    assert (
        daemon.request(
            "policy",
            "update",
            "--policy-id",
            policy_id,
            "--name",
            "delivery-v1",
            "--file",
            str(template_file),
        )
        == original
    )
    executable = tmp_path / "policy-target"
    shutil.copy2("/bin/sleep", executable)
    scope = daemon.request(
        "scope",
        "create",
        "--executable",
        str(executable),
        "--policy-id",
        policy_id,
        "--policy-revision",
        "1",
    )
    assert scope["policySnapshots"] == [original]
    assert daemon.request("binding", "list")["total"] == 0

    # Update before a process matches, so discovery must use the saved revision.
    next_template = {
        "specVersion": "0.1",
        "rules": [
            {
                "effect": "block",
                "category": "file",
                "action": "write",
                "target": {"type": "file", "path": "/new-policy"},
                "where": {"operation": {"eq": "delete"}},
            }
        ],
    }
    if structured:
        next_template = json.loads(json.dumps(template))
        next_template["fileProtection"]["targets"][0]["protections"][
            "ordinaryWrite"
        ] = False
    template_file.write_text(json.dumps(next_template))
    updated = daemon.request(
        "policy",
        "update",
        "--policy-id",
        policy_id,
        "--name",
        "delivery-v2",
        "--file",
        str(template_file),
    )
    assert updated["revision"] == 2
    process = subprocess.Popen([str(executable), "60"])
    try:
        stat = Path(f"/proc/{process.pid}/stat").read_text()
        identity = {
            "bootId": Path("/proc/sys/kernel/random/boot_id").read_text().strip(),
            "pidNamespace": os.readlink(f"/proc/{process.pid}/ns/pid"),
            "pid": process.pid,
            "startTime": int(stat.rsplit(")", 1)[1].split()[19]),
        }
        listing = _wait_for(
            lambda timeout: daemon.request("binding", "list", timeout=timeout),
            lambda value: value["total"] == 1
            and value["items"][0]["status"] == {"phase": "READY"},
        )
        binding = listing["items"][0]
        spec = binding["spec"]
        assert spec["policy"] == original
        assert spec["scope"] == {
            "scopeId": scope["scopeId"],
            "selector": scope["selector"],
            "process": identity,
        }
        assert spec["bindingRevision"] == 1
        expected = _fixture(_WIRE_FIXTURES / "apply.request.json")
        if structured:
            expected.pop("policy_dsl")
            expected["file_protection_policy"] = {
                "version": 1,
                "topology_mode": "static_at_apply",
                "targets": [
                    {
                        "path": "/workspace/protected",
                        "kind": "directory_tree",
                        "protections": {"delete": True, "ordinary_write": True},
                        "exclusions": [
                            {
                                "path": "/workspace/protected/cache",
                                "kind": "directory_subtree",
                            }
                        ],
                    }
                ],
            }
        expected.update(
            binding_id=str(
                uuid.uuid5(
                    uuid.NAMESPACE_URL,
                    f"urn:agentseccore:agentsight-binding:{spec['bindingId']}:revision:1",
                )
            ),
            root_pid=process.pid,
            process_start_time=identity["startTime"],
            policy_id=policy_id,
        )
        assert agentsight_mock.get(timeout=2) == (
            "GET",
            "/api/enforcement/health",
            None,
        )
        assert agentsight_mock.get(timeout=2) == ("POST", _BINDINGS_PATH, expected)
        assert agentsight_mock.empty(), "unexpected additional AgentSight request"

        # Read-only inspection confirms this actual daemon used durable storage.
        with closing(
            sqlite3.connect(f"file:{data / 'policy-state.db'}?mode=ro", uri=True)
        ) as db:
            saved = db.execute(
                "SELECT spec_json,phase FROM bindings WHERE binding_id=?",
                (spec["bindingId"],),
            ).fetchone()
            assert saved is not None
            assert (json.loads(saved[0]), saved[1]) == (spec, "READY")

        daemon.request("scope", "delete", "--scope-id", scope["scopeId"])
        _wait_for(
            lambda timeout: daemon.request("binding", "list", timeout=timeout),
            lambda value: value["total"] == 0,
        )
        _wait_for(
            lambda timeout: daemon.request("scope", "list", timeout=timeout),
            lambda value: value["total"] == 0,
        )
        assert agentsight_mock.get(timeout=2) == (
            "DELETE",
            f"{_BINDINGS_PATH}/{expected['binding_id']}",
            None,
        )
        assert agentsight_mock.empty(), "unexpected additional AgentSight request"
        assert (
            process.poll() is None
        ), "Scope deletion should clean up while the target remains alive"
        with closing(
            sqlite3.connect(f"file:{data / 'policy-state.db'}?mode=ro", uri=True)
        ) as db:
            assert db.execute("SELECT count(*) FROM bindings").fetchone() == (0,)
            assert db.execute("SELECT count(*) FROM scopes").fetchone() == (0,)
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def test_unsupported_rule_is_saved_but_prevents_partial_http_delivery(
    tmp_path: Path,
    agentsight_mock: Queue,
    start_daemon: Callable,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path / "policy-data"))
    daemon: DaemonHandle = start_daemon()
    template = _fixture(
        _V2 / "crates/asc-policy-types/tests/fixtures/prepared-binding.json"
    )["policy"]["template"]
    template["rules"][1]["effect"] = "require_confirmation"
    template_file = tmp_path / "review-policy.json"
    template_file.write_text(json.dumps(template))
    policy = daemon.request(
        "policy", "create", "--name", "review-required", "--file", str(template_file)
    )
    assert policy["template"] == template
    process = subprocess.Popen(["/bin/sleep", "60"])
    try:
        scope = daemon.request(
            "scope",
            "create",
            "--pid",
            str(process.pid),
            "--policy-id",
            policy["policyId"],
            "--policy-revision",
            "1",
        )
        listing = _wait_for(
            lambda timeout: daemon.request("binding", "list", timeout=timeout),
            lambda value: value["total"] == 1
            and value["items"][0]["status"]["phase"] == "APPLY_FAILED",
        )
        assert listing["items"][0]["spec"]["policy"] == policy
        assert listing["items"][0]["status"]["error"] == {
            "kind": "REJECTED",
            "code": "RULE_1_UNSUPPORTED_EFFECT",
        }
        assert (
            agentsight_mock.empty()
        ), "an unsupported rule must prevent all HTTP delivery"
        daemon.request("scope", "delete", "--scope-id", scope["scopeId"])
        _wait_for(
            lambda timeout: daemon.request("binding", "list", timeout=timeout),
            lambda value: value["total"] == 0,
        )
        assert (
            agentsight_mock.empty()
        ), "rejected translation must not create cleanup responsibility"
    finally:
        process.terminate()
        process.wait(timeout=5)


_LIVE_WORKER = r"""
import ctypes, json, os, sys
from pathlib import Path
libc = ctypes.CDLL(None, use_errno=True)
libc.renameat2.argtypes = [ctypes.c_int, ctypes.c_char_p,
                          ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
libc.renameat2.restype = ctypes.c_int
print(json.dumps({'errno': 0}), flush=True)
for line in sys.stdin:
    command = json.loads(line)
    op, path = command['op'], Path(command['path'])
    result = None
    try:
        if op == 'read':
            result = path.read_text()
        elif op == 'write':
            fd = os.open(path, os.O_WRONLY)
            try:
                os.write(fd, b'changed')
            finally:
                os.close(fd)
        elif op == 'create_empty':
            os.close(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600))
        elif op == 'unlink':
            path.unlink()
        elif op == 'rmdir':
            path.rmdir()
        elif op == 'rename':
            os.rename(path, command['destination'])
        elif op == 'replace':
            os.replace(path, command['destination'])
        elif op == 'exchange':
            if libc.renameat2(-100, os.fsencode(path), -100,
                              os.fsencode(command['destination']), 2) != 0:
                raise OSError(ctypes.get_errno(), 'exchange')
        else:
            raise ValueError(op)
        reply = {'errno': 0, 'result': result}
    except OSError as error:
        reply = {'errno': error.errno}
    print(json.dumps(reply), flush=True)
"""


def _live_namespace_guard() -> tuple[str, str, str]:
    namespaces = tuple(
        os.readlink(f"/proc/self/ns/{kind}") for kind in ("net", "mnt", "pid")
    )
    initial = tuple(os.readlink(f"/proc/1/ns/{kind}") for kind in ("net", "mnt", "pid"))
    if (
        namespaces[0] == initial[0]
        or namespaces[1] == initial[1]
        or namespaces[2] != initial[2]
    ):
        pytest.fail(
            "Live protection requires unshare --mount --net --propagation private, NO --pid"
        )
    if os.geteuid() != 0:
        pytest.fail("Live protection requires root in the isolated namespaces")
    if Path("/proc/self/ns/pid").stat().st_ino != _INITIAL_PID_NS_INODE:
        pytest.fail("Live protection refuses a nested PID namespace")
    for line in Path("/proc/self/mountinfo").read_text().splitlines():
        if any(
            field.startswith("shared:") for field in line.split(" - ")[0].split()[6:]
        ):
            pytest.fail("Live protection refuses shared mount propagation")
    return namespaces


def _live_wait(read: Callable, ready: Callable, description: str) -> Any:
    deadline = time.monotonic() + 30
    while (remaining := deadline - time.monotonic()) > 0:
        try:
            value = read(min(remaining, 1.0))
        except (OSError, URLError, subprocess.TimeoutExpired):
            value = None
        if value is not None and time.monotonic() < deadline and ready(value):
            return value
        time.sleep(min(0.05, max(0.0, deadline - time.monotonic())))
    pytest.fail(
        f"Live protection timed out: {description}; child logs are not disclosed"
    )


def _live_stop(process: subprocess.Popen) -> None:
    if process.poll() is None:
        process.send_signal(signal.SIGINT)
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


class _LiveDaemon(DaemonHandle):
    def request(self, *args: str, timeout: float = 30.0) -> dict[str, Any]:
        __tracebackhide__ = True
        result = self.cli(*args, timeout=timeout)
        if result.returncode != 0 or result.stderr:
            pytest.fail(
                "Owned daemon CLI request failed; diagnostics are not disclosed"
            )
        try:
            return json.loads(result.stdout)
        except json.JSONDecodeError:
            pytest.fail(
                "Owned daemon CLI reply is invalid; diagnostics are not disclosed"
            )


class _LiveWorker:
    def __init__(self, process: subprocess.Popen, output: Any) -> None:
        self.process, self.output = process, output
        self.pending = ""
        assert self._reply()["errno"] == 0

    def _reply(self) -> dict[str, Any]:
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            self.pending += self.output.readline()
            if "\n" in self.pending:
                line, self.pending = self.pending.split("\n", 1)
                try:
                    return json.loads(line)
                except json.JSONDecodeError:
                    pytest.fail(
                        "Owned worker reply is invalid; child logs are not disclosed"
                    )
            assert self.process.poll() is None, "Owned worker exited unexpectedly"
            time.sleep(0.02)
        pytest.fail("Owned worker response timed out")

    def command(
        self, op: str, path: Path, destination: Path | None = None
    ) -> dict[str, Any]:
        assert self.process.poll() is None
        command = {"op": op, "path": str(path)}
        if destination is not None:
            command["destination"] = str(destination)
        self.process.stdin.write(json.dumps(command) + "\n")
        self.process.stdin.flush()
        return self._reply()


def test_live_guard_rejects_outer_pid_even_if_proc1_matches(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        os, "readlink", lambda path: "pid:[1]" if path.endswith("/pid") else path
    )
    monkeypatch.setattr(os, "geteuid", lambda: 0)
    original_stat = Path.stat
    monkeypatch.setattr(
        Path,
        "stat",
        lambda path, *args, **kwargs: (
            SimpleNamespace(st_ino=1)
            if str(path) == "/proc/self/ns/pid"
            else original_stat(path, *args, **kwargs)
        ),
    )
    with pytest.raises(pytest.fail.Exception, match="nested PID namespace"):
        _live_namespace_guard()


def test_live_worker_waits_for_complete_reply_lines(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fragments = iter(['{"errno":0}', "\n", '{"errno":1}', "\n"])
    process = SimpleNamespace(poll=lambda: None)
    output = SimpleNamespace(readline=lambda: next(fragments, ""))
    monkeypatch.setattr(time, "sleep", lambda _: None)
    worker = _LiveWorker(process, output)
    assert worker._reply() == {"errno": 1}


@pytest.mark.parametrize(
    "reply",
    [
        subprocess.CompletedProcess([], 1, "private-output", "private-diagnostic"),
        subprocess.CompletedProcess([], 0, "private-invalid-json", ""),
    ],
)
def test_live_daemon_failures_do_not_disclose_diagnostics(
    monkeypatch: pytest.MonkeyPatch, reply: Any
) -> None:
    daemon = _LiveDaemon(None, Path("/unused"))
    monkeypatch.setattr(daemon, "cli", lambda *args, **kwargs: reply)
    with pytest.raises(pytest.fail.Exception) as failure:
        daemon.request("binding", "list")
    assert "private-" not in str(failure.value)


def test_live_wait_rejects_late_readiness(monkeypatch: pytest.MonkeyPatch) -> None:
    clock = iter([0, 1, 31, 32, 33])
    monkeypatch.setattr(time, "monotonic", lambda: next(clock))
    monkeypatch.setattr(time, "sleep", lambda _: None)
    with pytest.raises(pytest.fail.Exception, match="timed out"):
        _live_wait(lambda _: True, bool, "late response")


@pytest.fixture
def agentsight_live(
    monkeypatch: pytest.MonkeyPatch, daemon_settings: Callable
) -> Iterator:
    __tracebackhide__ = True
    if os.environ.get("ASC_FILE_PROTECTION_LIVE") != "1":
        pytest.skip("Set ASC_FILE_PROTECTION_LIVE=1 to run the isolated real chain")
    namespaces = _live_namespace_guard()
    binaries = {}
    for key in ("ASC_SIGHT_BIN", "ASC_ENFORCER_BIN", "ASC_CLI_BIN", "ASC_DAEMON_BIN"):
        path = Path(os.environ.get(key, ""))
        if not path.is_absolute() or not path.is_file() or not os.access(path, os.X_OK):
            pytest.fail(
                f"{key} must name an absolute executable from the tested candidate"
            )
        binaries[key] = str(path.resolve())
    monkeypatch.setattr(e2e, "CLI_BIN", binaries["ASC_CLI_BIN"])
    monkeypatch.setattr(e2e, "DAEMON_BIN", binaries["ASC_DAEMON_BIN"])
    targets = (Path("/var/log/sysak/.agentsight"), Path("/run/agentsight"))
    if any(not target.is_dir() or target.is_symlink() for target in targets):
        pytest.fail(
            "Pre-existing /run/agentsight and /var/log/sysak/.agentsight targets are required"
        )
    for utility in ("ip", "mount", "umount"):
        if shutil.which(utility) is None:
            pytest.fail(f"Live protection requires {utility}")
    temporary = tempfile.TemporaryDirectory(prefix="asc-live-", dir="/dev/shm")
    root = Path(temporary.name)
    pins = Path("/sys/fs/bpf") / f"asc-fp-{uuid.uuid4().hex}"
    pins_owned = False
    mounted, children = [], []
    daemon = worker = None
    scope_ids = []

    def spawn(argv: list[str], name: str, env: dict[str, str]) -> subprocess.Popen:
        with os.fdopen(
            os.open(root / f"{name}.log", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600),
            "w",
        ) as log:
            process = subprocess.Popen(
                argv,
                stdin=subprocess.PIPE,
                stdout=log,
                stderr=log,
                text=True,
                env=env,
                cwd=root,
            )
        children.append(process)
        return process

    def system(*args: str) -> None:
        subprocess.run(
            list(args),
            check=True,
            timeout=5,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

    try:
        system("ip", "link", "set", "lo", "up")
        for name, target in zip(("data", "locks"), targets):
            source = root / name
            source.mkdir(mode=0o700)
            system("mount", "--bind", str(source), str(target))
            mounted.append(target)
        assert (
            not pins.exists() and not pins.is_symlink()
        ), "Unique pin root unexpectedly exists"
        pins_owned = True
        environment = os.environ.copy()
        environment.update(
            AGENTSIGHT_ENFORCER_SOCKET=str(root / "enforcer.sock"),
            ACTPLANE_BPF_PIN_ROOT=str(pins),
            ACTPLANE_PINNED_PROFILE="agent-file-guard",
        )
        config = root / "sight.json"
        config.write_text(
            json.dumps(
                {
                    "schema_version": 5,
                    "server": {"auth": {"enabled": True}},
                    "storage": {"base_path": str(targets[0])},
                    "cmdline": {"deny": [{"rule": ["*"]}]},
                    "features": {
                        "token_stats": False,
                        "audit": False,
                        "token_consumption": False,
                        "sls_logtail": False,
                        "reuse_llm_judge": False,
                        "tokenizer": {"enabled": False},
                        "session_mapping": {"enabled": False},
                        "interruption_detection": {"enabled": False},
                        "trajectory_collection": {"enabled": False},
                    },
                }
            )
        )
        config.chmod(0o600)
        enforcer = spawn([binaries["ASC_ENFORCER_BIN"]], "enforcer", environment)
        sight = spawn(
            [
                binaries["ASC_SIGHT_BIN"],
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                "7396",
                "--db",
                str(targets[0] / "genai_events.db"),
                "--config",
                str(config),
            ],
            "sight",
            environment,
        )
        _live_wait(
            lambda _: _TOKEN_PATH.is_file() and _TOKEN_PATH.stat().st_size == 64,
            bool,
            "private auth token generation",
        )
        assert _TOKEN_PATH.stat().st_mode & 0o777 == 0o600
        authorization = "Bearer " + _TOKEN_PATH.read_text().strip()

        def http(path: str, timeout: float = 1.0) -> dict[str, Any]:
            __tracebackhide__ = True
            assert (
                enforcer.poll() is None and sight.poll() is None
            ), "Owned enforcement child exited"
            deadline = time.monotonic() + timeout
            with closing(
                HTTPConnection("127.0.0.1", 7396, timeout=timeout)
            ) as connection:
                connection.connect()
                owned_socket = connection.sock

                def expire() -> None:
                    try:
                        owned_socket.shutdown(socket.SHUT_RDWR)
                    except OSError:
                        pass

                timer = threading.Timer(max(0.0, deadline - time.monotonic()), expire)
                timer.start()
                try:
                    connection.request(
                        "GET", path, headers={"Authorization": authorization}
                    )
                    response = connection.getresponse()
                    if response.status != 200:
                        raise URLError("Owned HTTP request failed")
                    value = json.load(response)
                    if time.monotonic() >= deadline:
                        raise TimeoutError("Owned HTTP response exceeded its deadline")
                    return value
                except (HTTPException, json.JSONDecodeError):
                    raise URLError("Owned HTTP response failed") from None
                finally:
                    timer.cancel()
                    timer.join()

        _live_wait(
            lambda timeout: http("/api/auth/status", timeout),
            lambda value: value.get("auth_enabled") is True,
            "authenticated server startup",
        )
        health = _live_wait(
            lambda timeout: http("/api/enforcement/health", timeout),
            lambda value: value.get("ready") is True,
            "actual enforcer readiness",
        )
        assert health["backend"] == "actplane"
        capabilities = health["capabilities"]
        assert capabilities["test_development"] is False
        assert capabilities.get("file_delete_guard") is True
        for capability in (
            "file_entry_delete_protection",
            "directory_tree_delete_protection",
            "ordinary_write_protection",
            "static_mount_topology",
        ):
            assert (
                capabilities.get(capability) is True
            ), f"Required live capability absent: {capability}"
        for capability in ("complete_content_protection", "dynamic_mount_topology"):
            assert (
                capabilities.get(capability) is False
            ), f"Unsupported coverage advertised: {capability}"
        monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(root / "policy-data"))
        settings, daemon_environment = daemon_settings(root / "daemon.sock")
        daemon_environment["AGENT_SEC_DATA_DIR"] = str(root / "policy-data")
        process = spawn(
            [
                e2e.DAEMON_BIN,
                "--socket",
                str(root / "daemon.sock"),
                "--skillsec-config",
                str(settings),
                "--policy-admin-uid",
                str(os.getuid()),
            ],
            "daemon",
            daemon_environment,
        )
        daemon = _LiveDaemon(process, root / "daemon.sock")
        _live_wait(
            lambda timeout: daemon.cli("binding", "list", timeout=timeout),
            lambda result: result.returncode == 0,
            "real daemon UDS response",
        )
        process = spawn(
            [sys.executable, "-u", "-c", _LIVE_WORKER], "worker", environment
        )
        with (root / "worker.log").open() as output:
            worker = _LiveWorker(process, output)
            try:
                yield root, daemon, worker, http, scope_ids
            finally:
                try:
                    for scope_id in scope_ids[:]:
                        daemon.request("scope", "delete", "--scope-id", scope_id)
                        scope_ids.remove(scope_id)
                    _live_wait(
                        lambda timeout: daemon.request(
                            "binding", "list", timeout=timeout
                        ),
                        lambda value: value["total"] == 0,
                        "scope cleanup while worker is alive",
                    )
                    _live_wait(
                        lambda timeout: http(_BINDINGS_PATH, timeout),
                        lambda value: value["bindings"] == [],
                        "HTTP binding detach acknowledgement",
                    )
                    assert (
                        worker.process.poll() is None
                    ), "Worker exited before scope detach"
                except BaseException:
                    _live_stop(enforcer)
                    raise
    finally:
        errors = []
        for process in reversed(children):
            try:
                _live_stop(process)
                if process.stdin:
                    process.stdin.close()
            except (OSError, subprocess.TimeoutExpired):
                errors.append("owned child shutdown")
        if any(process.poll() is None for process in children):
            temporary._finalizer.detach()
            pytest.fail(
                "Owned children remain alive; preserving private mounts, pins and data"
            )
        current = tuple(
            os.readlink(f"/proc/self/ns/{kind}") for kind in ("net", "mnt", "pid")
        )
        if current != namespaces:
            temporary._finalizer.detach()
            pytest.fail("Namespace changed; refusing pin or mount cleanup")
        if pins_owned and pins.exists():
            try:
                shutil.rmtree(pins)
            except OSError:
                errors.append("owned pin cleanup")
        for target in mounted[:][::-1]:
            try:
                system("umount", str(target))
                mounted.remove(target)
            except (OSError, subprocess.SubprocessError):
                errors.append("private bind restoration")
        if mounted:
            temporary._finalizer.detach()
        else:
            temporary.cleanup()
        assert not errors, f"Live teardown failed: {errors}; no child logs disclosed"


def _live_snapshot(root: Path) -> dict:
    snapshot = {}
    for path in [root, *sorted(root.rglob("*"))]:
        stat = path.stat()
        content = (
            path.read_bytes()
            if path.is_file()
            else tuple(sorted(p.name for p in path.iterdir()))
        )
        snapshot[str(path.relative_to(root))] = (stat.st_dev, stat.st_ino, content)
    return snapshot


@pytest.mark.skipif(
    os.environ.get("ASC_FILE_PROTECTION_LIVE") != "1",
    reason="Opt-in real AgentSight/enforcer chain",
)
def test_live_file_protection_cli_uds_and_kernel_denials(
    agentsight_live: tuple,
) -> None:
    root, daemon, worker, http, scope_ids = agentsight_live
    files = root / "files"
    exact, tree = files / "exact-parent" / "delete-only", files / "tree"
    cache, empty = tree / "cache", tree / "empty"
    nested, outside = cache / "nested-exact", files / "outside"
    for directory in (exact.parent, cache, empty):
        directory.mkdir(parents=True, mode=0o700)
    for path in (exact, nested, outside, tree / "existing", cache / "allowed"):
        path.write_text("original")
    targets = [
        {
            "path": str(exact),
            "kind": "exact_file",
            "protections": {"delete": True, "ordinaryWrite": False},
        },
        {
            "path": str(tree),
            "kind": "directory_tree",
            "protections": {"delete": True, "ordinaryWrite": True},
            "exclusions": [{"path": str(cache), "kind": "directory_subtree"}],
        },
        {
            "path": str(nested),
            "kind": "exact_file",
            "protections": {"delete": True, "ordinaryWrite": True},
        },
    ]
    template = {
        "specVersion": "0.2",
        "fileProtection": {"topology": "static_at_apply", "targets": targets},
    }
    policy_file = root / "policy.json"
    policy_file.write_text(json.dumps(template))
    policy = daemon.request(
        "policy", "create", "--name", "live-file-protection", "--file", str(policy_file)
    )
    assert policy["template"] == template
    scope = daemon.request(
        "scope",
        "create",
        "--pid",
        str(worker.process.pid),
        "--policy-id",
        policy["policyId"],
        "--policy-revision",
        str(policy["revision"]),
    )
    scope_ids.append(scope["scopeId"])
    listing = _live_wait(
        lambda timeout: daemon.request("binding", "list", timeout=timeout),
        lambda value: value["total"] == 1
        and value["items"][0]["status"] == {"phase": "READY"},
        "CLI binding READY",
    )
    spec = listing["items"][0]["spec"]
    assert spec["policy"] == policy and spec["scope"]["scopeId"] == scope["scopeId"]
    wire_id = str(
        uuid.uuid5(
            uuid.NAMESPACE_URL,
            f"urn:agentseccore:agentsight-binding:{spec['bindingId']}:revision:{spec['bindingRevision']}",
        )
    )
    bindings = _live_wait(
        lambda timeout: http(_BINDINGS_PATH, timeout),
        lambda value: len(value["bindings"]) == 1
        and value["bindings"][0]["state"] == "enforced",
        "real HTTP binding enforcement",
    )
    request = bindings["bindings"][0]["request"]
    assert (
        request["binding_id"] == wire_id and request["root_pid"] == worker.process.pid
    )
    assert request["policy_id"] == policy["policyId"]
    assert request["policy_revision"] == str(policy["revision"])
    assert request["process_start_time"] == spec["scope"]["process"]["startTime"]
    for path in (exact, nested, tree / "existing"):
        assert worker.command("read", path) == {"errno": 0, "result": "original"}

    def denied(
        op: str, path: Path, destination: Path | None = None, evidence: bool = False
    ) -> None:
        before = _live_snapshot(files)
        previous = {
            event["event_id"]
            for event in http("/api/enforcement/violations?limit=1000")["violations"]
        }
        assert (
            worker.command(op, path, destination)["errno"] == errno.EPERM
        ), f"{op} must fail with EPERM"
        assert _live_snapshot(files) == before, f"{op} changed the protected filesystem"
        if evidence:
            operation = "ordinary_write" if op == "write" else op
            _live_wait(
                lambda timeout: http("/api/enforcement/violations?limit=1000", timeout),
                lambda value: any(
                    event["event_id"] not in previous
                    and event["binding_id"] == wire_id
                    and event["policy_id"] == policy["policyId"]
                    and event["policy_revision"] == str(policy["revision"])
                    and event["pid"] == worker.process.pid
                    and event["process_start_time"] == request["process_start_time"]
                    and event["blocked"] is True
                    and event["effect"] == "block"
                    and event["operation"] == operation
                    and event["target"] in (str(path), path.name)
                    and bool(event["rule_id"])
                    and bool(event["reason"])
                    for event in value["violations"]
                ),
                "correlated real violation evidence",
            )

    denied("unlink", exact, evidence=True)
    denied("write", tree / "existing", evidence=True)
    denied("rmdir", empty, evidence=True)
    denied("rmdir", tree)
    denied("unlink", nested)
    denied("write", nested)
    future = tree / "future" / "descendant"
    future.parent.mkdir(mode=0o700)
    assert worker.command("create_empty", future)["errno"] == 0
    denied("write", future)
    denied("unlink", future)
    denied("rename", exact.parent, files / "moved-parent")
    denied("rename", tree, files / "moved-tree")
    for op in ("replace", "exchange"):
        for source, destination in (
            (outside, exact),
            (exact, outside),
            (outside, nested),
            (nested, outside),
        ):
            denied(op, source, destination)
    old_inode = exact.stat().st_ino
    os.replace(outside, exact)
    assert exact.stat().st_ino != old_inode and not outside.exists()
    denied("unlink", exact)
    assert worker.command("write", exact)["errno"] == 0
    for directory in (files / "outside-dir", cache / "legitimate"):
        directory.mkdir(mode=0o700)
        path = directory / "allowed"
        assert worker.command("create_empty", path)["errno"] == 0
        assert worker.command("write", path)["errno"] == 0
        assert path.read_bytes() == b"changed"
        peer = directory / "peer"
        peer.write_text("peer")
        assert worker.command("exchange", path, peer)["errno"] == 0
        assert path.read_text() == "peer" and peer.read_bytes() == b"changed"
        assert worker.command("replace", peer, path)["errno"] == 0
        assert not peer.exists() and path.read_bytes() == b"changed"
        assert worker.command("rename", path, directory / "renamed")["errno"] == 0
        assert worker.command("unlink", directory / "renamed")["errno"] == 0
        assert worker.command("rmdir", directory)["errno"] == 0
    daemon.request("scope", "delete", "--scope-id", scope["scopeId"])
    scope_ids.remove(scope["scopeId"])
    _live_wait(
        lambda timeout: daemon.request("binding", "list", timeout=timeout),
        lambda value: value["total"] == 0,
        "CLI binding absence before worker exit",
    )
    _live_wait(
        lambda timeout: http(_BINDINGS_PATH, timeout),
        lambda value: value["bindings"] == [],
        "HTTP detach before worker exit",
    )
    for path in (exact, nested, future, tree / "existing"):
        assert worker.command("write", path)["errno"] == 0
        assert worker.command("unlink", path)["errno"] == 0
        assert not path.exists()
    assert worker.command("rmdir", empty)["errno"] == 0
    assert worker.process.poll() is None
