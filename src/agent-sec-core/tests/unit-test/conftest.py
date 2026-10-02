"""Global fixtures for unit tests — isolate all security-event I/O."""

import os

import agent_sec_cli.security_events as _security_events
import pytest


@pytest.fixture(autouse=True, scope="session")
def _isolate_data_dir(tmp_path_factory):
    """Redirect all security-event I/O to a disposable temp directory.

    Prevents unit tests from polluting the production SQLite DB / JSONL log
    at ``~/.agent-sec-core/``.
    """
    data_dir = tmp_path_factory.mktemp("unit-test-data")
    os.environ["AGENT_SEC_DATA_DIR"] = str(data_dir)

    # Reset module-level singletons to force re-initialization with new data dir.
    # This prevents test pollution when singletons were lazily initialized
    # before this fixture ran (e.g., during module import phase).
    _security_events._writer = None
    _security_events._sqlite_writer = None
    _security_events._reader = None

    yield

    # Cleanup: reset singletons and remove env var
    _security_events._writer = None
    _security_events._sqlite_writer = None
    _security_events._reader = None
    os.environ.pop("AGENT_SEC_DATA_DIR", None)
