"""Acquired fixed-port containers are cleaned when startup/stop fails."""

import importlib.util
import sys
from pathlib import Path
from types import ModuleType, SimpleNamespace

import pytest


@pytest.fixture
def sandbox(monkeypatch: pytest.MonkeyPatch):
    events, failures, warnings = [], {}, []

    def action(name, result=None):
        def called(*args, **kwargs):
            events.append((name, args, kwargs))
            if name in failures:
                raise failures[name]
            return result

        return called

    container = SimpleNamespace(stop=action("stop"), remove=action("remove"))
    client = SimpleNamespace(
        containers=SimpleNamespace(run=action("create", container))
    )
    for name in (
        "ce_runner",
        "ce_runner._common",
        "docker",
        "claw_eval",
        "claw_eval.runner",
        "claw_eval.runner.sandbox_runner",
    ):
        monkeypatch.setitem(sys.modules, name, ModuleType(name))
    sys.modules["ce_runner._common"].log = warnings.append
    sys.modules["docker"].from_env = lambda: client
    sys.modules["claw_eval.runner.sandbox_runner"].ContainerHandle = lambda **kwargs: (
        SimpleNamespace(**kwargs)
    )
    path = Path(__file__).parents[1] / "src/ce_runner/sandbox_helpers.py"
    spec = importlib.util.spec_from_file_location("ce_runner.sandbox_helpers", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    monkeypatch.setattr(module, "_wait_healthy", action("health"))
    monkeypatch.setattr(module, "_probe_exec", action("probe"))
    monkeypatch.setattr(module.time, "sleep", action("sleep"))
    return SimpleNamespace(
        module=module,
        events=events,
        failures=failures,
        warnings=warnings,
        container=container,
    )


@pytest.mark.parametrize("stage", ["health", "probe"])
@pytest.mark.parametrize("error_type", [TimeoutError, KeyboardInterrupt])
def test_startup_failure_removes_the_created_container(sandbox, stage, error_type):
    failure = error_type("startup failed")
    sandbox.failures[stage] = failure
    with pytest.raises(error_type) as raised:
        sandbox.module.start_sandbox_container("image", "trial", 9200)
    assert raised.value is failure
    assert [name for name, _, _ in sandbox.events if name in {"stop", "remove"}] == [
        "stop",
        "remove",
    ]
    assert next(kwargs for name, _, kwargs in sandbox.events if name == "remove") == {
        "force": True
    }


def test_successful_start_retains_its_owned_container(sandbox):
    handle = sandbox.module.start_sandbox_container("image", "trial", 9200)
    assert handle.container is sandbox.container
    assert handle.sandbox_url == "http://localhost:9200"
    assert handle.host_port == 9200
    assert [name for name, _, _ in sandbox.events] == ["create", "health", "probe"]


def test_creation_failure_has_no_container_to_remove(sandbox):
    failure = RuntimeError("create failed")
    sandbox.failures["create"] = failure
    with pytest.raises(RuntimeError) as raised:
        sandbox.module.start_sandbox_container("image", "trial", 9200)
    assert raised.value is failure
    assert [name for name, _, _ in sandbox.events] == ["create"]


def test_failed_stop_still_forces_removal(sandbox):
    sandbox.failures["stop"] = RuntimeError("stop failed")
    handle = SimpleNamespace(container=sandbox.container, run_id="trial")
    sandbox.module.stop_sandbox_container(handle)
    assert [name for name, _, _ in sandbox.events] == ["stop", "remove", "sleep"]
    assert any("stop failed" in warning for warning in sandbox.warnings)


def test_failed_removal_is_reported_without_claiming_success(sandbox):
    sandbox.failures["remove"] = RuntimeError("remove failed")
    sandbox.module.stop_sandbox_container(
        SimpleNamespace(container=sandbox.container, run_id="trial")
    )
    assert [name for name, _, _ in sandbox.events] == ["stop", "remove"]
    assert any("remove failed" in warning for warning in sandbox.warnings)
    assert not any(" removed" in warning for warning in sandbox.warnings)


def test_cleanup_failure_does_not_replace_startup_failure(sandbox):
    failure = TimeoutError("health failed")
    sandbox.failures.update(
        health=failure,
        stop=RuntimeError("stop failed"),
        remove=RuntimeError("remove failed"),
    )
    with pytest.raises(TimeoutError) as raised:
        sandbox.module.start_sandbox_container("image", "trial", 9200)
    assert raised.value is failure
    assert [name for name, _, _ in sandbox.events] == [
        "create",
        "health",
        "stop",
        "remove",
    ]
    assert any("remove failed" in warning for warning in sandbox.warnings)


def test_successful_shutdown_retains_stop_remove_grace_order(sandbox):
    sandbox.module.stop_sandbox_container(
        SimpleNamespace(container=sandbox.container, run_id="trial")
    )
    assert [name for name, _, _ in sandbox.events] == ["stop", "remove", "sleep"]


@pytest.mark.parametrize("stage", ["stop", "remove"])
def test_shutdown_interrupt_is_rethrown_after_cleanup_attempts(sandbox, stage):
    failure = KeyboardInterrupt("shutdown interrupted")
    sandbox.failures[stage] = failure
    with pytest.raises(KeyboardInterrupt) as raised:
        sandbox.module.stop_sandbox_container(
            SimpleNamespace(container=sandbox.container, run_id="trial")
        )
    assert raised.value is failure
    assert [name for name, _, _ in sandbox.events if name in {"stop", "remove"}] == [
        "stop",
        "remove",
    ]


@pytest.mark.parametrize("stage", ["stop", "remove"])
def test_startup_failure_remains_primary_after_cleanup_interrupt(sandbox, stage):
    failure = TimeoutError("startup failed")
    sandbox.failures.update(health=failure)
    sandbox.failures[stage] = KeyboardInterrupt("cleanup interrupted")
    with pytest.raises(TimeoutError) as raised:
        sandbox.module.start_sandbox_container("image", "trial", 9200)
    assert raised.value is failure
    assert [name for name, _, _ in sandbox.events if name in {"stop", "remove"}] == [
        "stop",
        "remove",
    ]
