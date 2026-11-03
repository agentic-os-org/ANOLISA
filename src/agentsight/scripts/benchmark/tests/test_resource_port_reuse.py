"""A closed reusable server port must not stop the real resource coordinator."""

import errno
import json
import socket
import sys
from pathlib import Path

import pytest
from test_resource_run import FAKE_TRACE, assert_reaped, command
from test_resource_run import environment as environment
from test_resource_run import executable, run

pytestmark = pytest.mark.skipif(sys.platform != "linux", reason="resource tracing requires Linux")


def leave_time_wait(port: int) -> None:
    with socket.socket() as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", port))
        listener.listen()
        client = socket.create_connection(("127.0.0.1", port))
        peer, _ = listener.accept()
        peer.shutdown(socket.SHUT_WR)
        peer.close()
        assert client.recv(1) == b""
        client.close()
    # This is a real kernel state, not a mocked bind failure.
    with socket.socket() as ordinary:
        with pytest.raises(OSError) as error:
            ordinary.bind(("127.0.0.1", port))
        assert error.value.errno == errno.EADDRINUSE


@pytest.mark.parametrize("ab", [False, True], ids=["single", "ab"])
def test_real_cli_reuses_a_closed_server_port(
    tmp_path: Path, environment: dict[str, str], ab: bool
) -> None:
    binary = executable(tmp_path / "observer", FAKE_TRACE.replace("MEMORY_MIB", "4"))
    output = tmp_path / "result"
    arguments = command(output, binary)
    port = int(arguments[arguments.index("--port") + 1])
    leave_time_wait(port)
    if ab:
        arguments += ["--ab", "--baseline-bin", str(binary)]
    result = run(arguments, environment)
    assert result.returncode == 0, result.stdout + result.stderr
    summary = json.loads((output / "resource-summary.json").read_text())
    assert summary["valid"]
    assert list(summary["runs"]) == (["before", "after"] if ab else ["run"])
    assert all(record["capture"] == "PASS" for record in summary["runs"].values())
    assert_reaped(output)


def test_real_cli_still_refuses_a_live_listener(
    tmp_path: Path, environment: dict[str, str]
) -> None:
    binary = executable(tmp_path / "observer", FAKE_TRACE.replace("MEMORY_MIB", "4"))
    output = tmp_path / "result"
    arguments = command(output, binary)
    port = int(arguments[arguments.index("--port") + 1])
    with socket.socket() as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", port))
        listener.listen()
        result = run(arguments, environment)
        assert result.returncode == 1
        assert "Address already in use" in result.stdout
        assert listener.getsockopt(socket.SOL_SOCKET, socket.SO_ACCEPTCONN) == 1
    assert not list(output.glob("*/observer-start.json"))
    assert_reaped(output)
