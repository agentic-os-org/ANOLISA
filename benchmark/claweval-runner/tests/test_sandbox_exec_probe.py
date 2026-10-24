"""Exercise sandbox readiness using offline HTTP responses and retry clocks."""

import sys
from pathlib import Path
from unittest.mock import Mock, patch

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
from ce_runner import sandbox_helpers


@pytest.mark.parametrize(
    "body",
    [
        {"stdout": "not okay"},
        {"stdout": "ok but execution failed"},
        {"stdout": ["ok"]},
        {"stdout": {"ok": True}},
        {"stdout": "ok", "error": "executor unavailable"},
        {"stdout": "ok", "status": "error"},
        {"stdout": "ok", "isError": True},
        {"stdout": None},
        {},
        None,
        [],
    ],
)
def test_invalid_exec_response_is_not_ready(body: object) -> None:
    response = Mock(status_code=200)
    response.json.return_value = body
    with patch.object(sandbox_helpers.httpx, "post", return_value=response) as post:
        with pytest.raises(TimeoutError, match="after 1 attempts"):
            sandbox_helpers._probe_exec("http://fixture.invalid", max_attempts=1)
    post.assert_called_once_with(
        "http://fixture.invalid/exec", json={"command": "echo ok", "timeout_seconds": 5}, timeout=10
    )


@pytest.mark.parametrize("stdout", ["ok", "ok\n", " \tok\r\n"])
def test_exact_echo_output_is_ready(stdout: str) -> None:
    response = Mock(status_code=200)
    response.json.return_value = {"stdout": stdout}
    with patch.object(sandbox_helpers.httpx, "post", return_value=response):
        with patch.object(sandbox_helpers.time, "sleep") as sleep:
            sandbox_helpers._probe_exec("http://fixture.invalid")
    sleep.assert_not_called()


@pytest.mark.parametrize("attempts,waits", [(5, [1, 2, 4, 4]), (8, [1, 2, 4, 4, 4, 4, 4])])
def test_retry_budget_is_bounded(attempts: int, waits: list[int]) -> None:
    response = Mock(status_code=503)
    with patch.object(sandbox_helpers.httpx, "post", return_value=response) as post:
        with patch.object(sandbox_helpers.time, "sleep") as sleep:
            with pytest.raises(TimeoutError, match=f"after {attempts} attempts"):
                sandbox_helpers._probe_exec("http://fixture.invalid", max_attempts=attempts)
    assert post.call_count == attempts
    assert [call.args[0] for call in sleep.call_args_list] == waits


def test_transient_failure_can_recover() -> None:
    response = Mock(status_code=200)
    response.json.return_value = {"stdout": "ok\n", "error": None, "isError": False}
    with patch.object(sandbox_helpers.httpx, "post", side_effect=[OSError("offline"), response]):
        with patch.object(sandbox_helpers.time, "sleep") as sleep:
            sandbox_helpers._probe_exec("http://fixture.invalid")
    sleep.assert_called_once_with(1)
