"""Global test fixtures for agent-sec-core."""

import os
import sys
import tempfile
from pathlib import Path

import pytest


@pytest.fixture(autouse=True)
def v2_cli_runtime(request, monkeypatch):
    """Provide an isolated V2 daemon to the V1 event and report cases."""
    runtime = {
        "test_events_e2e.py": "SEC_EVENTS_E2E_RUNTIME",
        "test_session_report_e2e.py": "OBS_E2E_RUNTIME",
    }.get(request.node.path.name)
    cli_tests = Path(__file__).parent / "e2e/cli"
    if request.node.path.parent != cli_tests or os.environ.get(runtime or "") != "v2":
        yield
        return

    # V1's pytest entry does not expose the tests namespace during collection.
    from tests.v2.e2e import conftest as v2_e2e  # noqa: PLC0415

    request.getfixturevalue("isolated_data_dir")
    # V2's Policy repository requires private daemon-owned data directories.
    Path(os.environ["AGENT_SEC_DATA_DIR"]).chmod(0o700)
    v2_e2e._require("agent-sec-cli")
    with tempfile.TemporaryDirectory(prefix="asc-cli-", dir="/tmp") as directory:
        socket_path = Path(directory) / "daemon.sock"
        monkeypatch.setenv("AGENT_SEC_DAEMON_SOCKET", str(socket_path))
        process = v2_e2e._start_daemon(socket_path, [])
        try:
            yield
        finally:
            v2_e2e._terminate(process)
            assert process.returncode == 0
            assert not socket_path.exists()


def pytest_configure(config: pytest.Config) -> None:
    """Use a short basetemp on macOS to avoid AF_UNIX socket path length limit.

    macOS limits AF_UNIX socket paths to 104 bytes. pytest's default basetemp
    on macOS is under /private/var/folders/... which can exceed this limit.

    Placed at tests/ root so all subdirectories (unit-test, e2e, integration-test)
    benefit — tmp_path_factory in unit-test/conftest.py also produces short paths.

    /tmp/agd-pytest-<uid> is used instead of tempfile.gettempdir() because the
    latter returns /private/var/folders/... on macOS, which is exactly the long
    path we want to avoid. Residual directories are managed by pytest's built-in
    basetemp cleanup logic (keeps last 3 runs, removes older ones).
    """
    if sys.platform == "darwin" and not config.option.basetemp:
        basetemp = Path(f"/tmp/agd-pytest-{os.getuid()}")  # noqa: S108
        basetemp.mkdir(parents=True, exist_ok=True)
        basetemp.chmod(0o700)
        config.option.basetemp = basetemp
