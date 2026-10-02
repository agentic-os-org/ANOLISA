"""Global test fixtures for agent-sec-core."""

import os
import sys
from pathlib import Path

import pytest


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
