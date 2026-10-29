"""Preserve the startup failure when forced process cleanup also fails."""

from types import SimpleNamespace

import pytest
from tests.v2.e2e import conftest as daemon_fixtures


def test_readiness_timeout_keeps_its_context_when_cleanup_times_out(
    monkeypatch, tmp_path
):
    timeout = daemon_fixtures._SOCKET_WAIT_SECONDS
    times = iter([0.0, timeout, timeout])
    monkeypatch.setattr(
        daemon_fixtures, "time", SimpleNamespace(monotonic=lambda: next(times))
    )
    monkeypatch.setattr(daemon_fixtures, "_daemon_settings", lambda path: (path, {}))
    monkeypatch.setattr(daemon_fixtures, "_require", lambda name: name)
    monkeypatch.setattr(
        daemon_fixtures,
        "subprocess",
        SimpleNamespace(
            Popen=lambda *args, **kwargs: SimpleNamespace(pid=123), PIPE=-1
        ),
    )

    def cleanup(process):
        raise AssertionError("shutdown window expired; retained startup diagnostics")

    monkeypatch.setattr(daemon_fixtures, "_terminate", cleanup)
    with pytest.raises(AssertionError) as failure:
        daemon_fixtures._start_daemon(tmp_path / "daemon.sock", [])

    message = str(failure.value)
    assert f"did not become healthy after {timeout:.3f}s" in message
    assert "pid=123" in message
    assert "last_probe=socket not created" in message
    assert "shutdown window expired" in message
    assert "retained startup diagnostics" in message
