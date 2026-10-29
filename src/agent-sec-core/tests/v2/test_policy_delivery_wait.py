"""Bound lifecycle convergence without retrying failed CLI reads."""

import subprocess
from types import SimpleNamespace

import pytest
from tests.v2.crash_consistency import conftest as recovery
from tests.v2.e2e import test_policy_delivery_e2e as delivery


@pytest.fixture(params=[delivery, recovery], ids=["delivery", "recovery"])
def clock(request, monkeypatch):
    now = [0.0]

    def advance(seconds):
        now[0] += seconds

    module = request.param
    wait = delivery._wait_for if module is delivery else recovery.until
    clock = SimpleNamespace(monotonic=lambda: now[0], sleep=advance, wait=wait)
    monkeypatch.setattr(module, "time", clock)
    return clock


def test_cli_timeout_is_not_retried(clock):
    calls = []

    def read(timeout):
        calls.append(timeout)
        clock.sleep(timeout)
        raise subprocess.TimeoutExpired(["agent-sec-cli", "binding", "list"], timeout)

    with pytest.raises(subprocess.TimeoutExpired):
        clock.wait(read, lambda value: value["total"] == 0)

    assert calls == [pytest.approx(30)]
    assert clock.monotonic() == pytest.approx(30)


def test_state_polling_uses_the_remaining_overall_budget(clock):
    budgets = []
    expected = {"total": 0}

    def read(timeout):
        budgets.append(timeout)
        if len(budgets) == 1:
            clock.sleep(3)
            return {"total": 1}
        return expected

    assert clock.wait(read, lambda value: value["total"] == 0) == expected
    assert budgets == pytest.approx([30, 30 - clock.monotonic()])


def test_convergence_timeout_preserves_the_last_observed_state(clock):
    observed = {"items": [{"status": {"phase": "APPLYING"}}]}

    def read(timeout):
        clock.sleep(timeout)
        return observed

    with pytest.raises(pytest.fail.Exception) as failure:
        clock.wait(read, lambda value: False)

    assert "APPLYING" in str(failure.value)
    assert clock.monotonic() == pytest.approx(30)


def test_non_timeout_failure_is_not_retried(clock):
    def read(timeout):
        raise AssertionError("policy query failed")

    with pytest.raises(AssertionError, match="policy query failed"):
        clock.wait(read, lambda value: False)
    assert clock.monotonic() == 0
