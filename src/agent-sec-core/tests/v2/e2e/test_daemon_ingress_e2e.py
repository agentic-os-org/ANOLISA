"""DPROC-UDS-002: real kernel UID isolation and administrator connection reserve."""

import json
import os
import socket
import subprocess
import tempfile
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

HEALTH_REQUEST = b'{"method":"daemon.health","params":{}}\n'
UID_CLIENT = r"""
import json, socket, sys

endpoint, count = sys.argv[1], int(sys.argv[2])

def read_response(stream):
    data = b""
    while not data.endswith(b"\n"):
        chunk = stream.recv(4096)
        if not chunk:
            break
        data += chunk
        assert len(data) < 65536
    return json.loads(data)

def request(payload):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(2)
        stream.connect(endpoint)
        stream.sendall(payload)
        return read_response(stream)

held = []
try:
    for _ in range(count):
        stream = socket.socket(socket.AF_UNIX)
        stream.settimeout(2)
        stream.connect(endpoint)
        stream.sendall(b'{"method":')
        held.append(stream)
    # Forged attribution cannot acquire a different UID quota or the reserve.
    payload = (b'{"method":"daemon.health","params":{},"uid":0,"role":"PolicyAdministrator"}\n'
               if count else b'{"method":"daemon.health","params":{}}\n')
    print(json.dumps(request(payload)), flush=True)
    if count:
        assert sys.stdin.readline() == "release\n"
        for stream in held:
            stream.sendall(b'"daemon.health","params":{}}\n')
            assert read_response(stream)["result"]["status"] == "ok"
        print(json.dumps(request(b'{"method":"daemon.health","params":{}}\n')), flush=True)
finally:
    for stream in held:
        stream.close()
"""


def _uid_client(endpoint: Path, uid: int, count: int) -> subprocess.Popen[str]:
    return subprocess.Popen(
        ["/usr/bin/python3", "-c", UID_CLIENT, str(endpoint), str(count)],
        user=uid,
        group=uid,
        extra_groups=[],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def _request(endpoint: Path) -> dict:
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(2)
        stream.connect(str(endpoint))
        stream.sendall(HEALTH_REQUEST)
        with stream.makefile("rb") as response:
            return json.loads(response.readline())


def _uid_request(endpoint: Path, uid: int) -> dict:
    process = _uid_client(endpoint, uid, 0)
    stdout, stderr = process.communicate(timeout=3)
    assert process.returncode == 0, stderr
    return json.loads(stdout)


@pytest.mark.skipif(os.geteuid() != 0, reason="cross-UID UDS test requires root")
def test_dproc_uds_002_uid_isolation_administrator_reserve_and_recovery(
    start_daemon: Callable[..., Any],
) -> None:
    clients = []
    reserved = []
    with tempfile.TemporaryDirectory(prefix="asc-ingress-", dir="/tmp") as directory:
        runtime = Path(directory)
        runtime.chmod(0o755)
        administrator = 10008
        daemon = start_daemon(
            admin_uids=[administrator], name=str(runtime / "daemon.sock")
        )
        try:
            for uid in range(10001, 10005):
                process = _uid_client(daemon.socket_path, uid, 16)
                clients.append(process)
                ready_line = process.stdout.readline()
                assert ready_line, process.stderr.read()
                ready = json.loads(ready_line)
                assert ready["error"]["code"] == "resource_exhausted", ready
                assert (
                    ready["error"]["message"]
                    == "daemon connection capacity is exhausted"
                )
                if len(clients) == 1:
                    # A saturated UID leaves capacity for another ordinary UID.
                    assert (
                        _uid_request(daemon.socket_path, 10009)["result"]["status"]
                        == "ok"
                    )

            # This test fills the 64-slot public pool using four UIDs at 16 each.
            assert (
                _uid_request(daemon.socket_path, 10009)["error"]["code"]
                == "resource_exhausted"
            )
            assert _request(daemon.socket_path)["result"]["status"] == "ok"
            assert (
                _uid_request(daemon.socket_path, administrator)["result"]["status"]
                == "ok"
            )

            for _ in range(16):
                stream = socket.socket(socket.AF_UNIX)
                stream.settimeout(2)
                stream.connect(str(daemon.socket_path))
                stream.sendall(b'{"method":')
                reserved.append(stream)
            # Root also receives busy when all 80 admitted connections are held.
            assert _request(daemon.socket_path)["error"]["code"] == "resource_exhausted"
            for stream in reserved:
                stream.sendall(b'"daemon.health","params":{}}\n')
                with stream.makefile("rb") as response:
                    assert json.loads(response.readline())["result"]["status"] == "ok"
            assert _request(daemon.socket_path)["result"]["status"] == "ok"

            for process in clients:
                stdout, stderr = process.communicate("release\n", timeout=3)
                assert process.returncode == 0, stderr
                assert json.loads(stdout)["result"]["status"] == "ok"
            assert _uid_request(daemon.socket_path, 10009)["result"]["status"] == "ok"
        finally:
            for stream in reserved:
                stream.close()
            for process in clients:
                if process.poll() is None:
                    process.kill()
                process.communicate(timeout=3)
            daemon.process.terminate()
            daemon.process.communicate(timeout=5)
            assert daemon.process.returncode == 0
