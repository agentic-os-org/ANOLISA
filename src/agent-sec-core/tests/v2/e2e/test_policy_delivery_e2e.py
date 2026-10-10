"""Real CLI/daemon delivery to an HTTP mock at the default AgentSight address.

Run in the isolated root E2E environment with port 7396 available. This exercises
the production Client and SQLite repository, not AgentSight or kernel enforcement.
"""

import json
import os
import shutil
import sqlite3
import subprocess
import threading
import time
import uuid
from collections.abc import Callable, Iterator
from contextlib import closing
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from queue import Queue
from typing import Any

import pytest
from tests.v2.e2e.conftest import DaemonHandle

_V2 = Path(__file__).resolve().parents[3] / "v2"
_WIRE_FIXTURES = _V2 / "fixtures/clients/agentsight/file-deletion"
_TOKEN_PATH = Path("/var/log/sysak/.agentsight/.dashboard_token")
_BINDINGS_PATH = "/api/enforcement/bindings"


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
                    self._reply(200, _fixture(_WIRE_FIXTURES / "health.response.json"))
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
    read: Callable[[], dict[str, Any]], ready: Callable[[dict[str, Any]], bool]
) -> dict[str, Any]:
    deadline = time.monotonic() + 10
    while True:
        actual = read()
        if ready(actual):
            return actual
        if time.monotonic() >= deadline:
            pytest.fail(f"Policy lifecycle did not converge: {actual}")
        time.sleep(0.05)


def test_discovered_binding_delivers_saved_revision_and_cleans_up(
    tmp_path: Path,
    agentsight_mock: Queue,
    start_daemon: Callable,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    data = tmp_path / "policy-data"
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(data))
    daemon: DaemonHandle = start_daemon()
    template = _fixture(
        _V2 / "crates/asc-policy-types/tests/fixtures/prepared-binding.json"
    )["policy"]["template"]
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
    template_file.write_text(
        json.dumps(
            {
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
        )
    )
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
            lambda: daemon.request("binding", "list", timeout=2),
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
            lambda: daemon.request("binding", "list", timeout=2),
            lambda value: value["total"] == 0,
        )
        _wait_for(
            lambda: daemon.request("scope", "list", timeout=2),
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
            lambda: daemon.request("binding", "list", timeout=2),
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
            lambda: daemon.request("binding", "list"), lambda value: value["total"] == 0
        )
        assert (
            agentsight_mock.empty()
        ), "rejected translation must not create cleanup responsibility"
    finally:
        process.terminate()
        process.wait(timeout=5)
