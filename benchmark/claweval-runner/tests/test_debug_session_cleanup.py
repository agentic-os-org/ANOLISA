"""Exercise the real debug session lifecycle without external services."""

import importlib.util
import signal
import sys
import threading
from pathlib import Path
from types import ModuleType, SimpleNamespace

import pytest


@pytest.fixture
def debug_session(monkeypatch: pytest.MonkeyPatch, tmp_path: Path):
    events = []
    failures = {}
    handle = SimpleNamespace(sandbox_url="http://sandbox.invalid", run_id="test")
    ctx = SimpleNamespace(mcp_name="mock", sandbox_mcp_name="sandbox")

    def action(name, result=None):
        def call(*args, **kwargs):
            events.append((name, args, kwargs))
            if name in failures:
                raise failures[name]
            return result

        return call

    modules = {}
    for name in (
        "ce_runner",
        "ce_runner._common",
        "ce_runner.infra",
        "ce_runner.run_task",
        "claw_eval",
        "claw_eval.config",
        "claw_eval.models",
        "claw_eval.models.task",
        "claw_eval.runner",
        "claw_eval.runner.sandbox_runner",
    ):
        modules[name] = ModuleType(name)
        monkeypatch.setitem(sys.modules, name, modules[name])
    modules["ce_runner"].__path__ = [str(Path(__file__).parents[1] / "src/ce_runner")]
    common = modules["ce_runner._common"]
    common.OPENCLAW_CONFIG = tmp_path / "config.json"
    common._REPO_DIR = tmp_path
    common._PYTHON = sys.executable
    common.load_config = action("load_config", {})
    common.load_task_yaml = action("load_task", {"task_id": "T001", "prompt": "hello"})
    common.log = action("log")
    infra = modules["ce_runner.infra"]
    for name, result in (
        ("configure_tools", ("agent", None, ctx)),
        ("cleanup_mock_services", None),
        ("reset_services", None),
        ("start_mock_services", None),
        ("restart_gateway", True),
        ("cleanup_config", None),
        ("check_gateway", 1234),
    ):
        setattr(infra, name, action(name, result))
    modules["ce_runner.run_task"].get_model_config = action("model_config", {})
    modules["claw_eval.config"].SandboxConfig = lambda **kwargs: SimpleNamespace(
        **kwargs
    )
    modules["claw_eval.models.task"].TaskDefinition = SimpleNamespace(
        from_yaml=action("task_definition", SimpleNamespace(sandbox_files=[]))
    )
    runner = SimpleNamespace(
        start_container=action("start_container", handle),
        inject_files=action("inject_files", 0),
        stop_container=action("stop_container"),
    )
    modules["claw_eval.runner.sandbox_runner"].SandboxRunner = lambda cfg: runner
    monkeypatch.setattr(sys, "path", list(sys.path))
    path = Path(__file__).parents[1] / "scripts" / "debug_task.py"
    spec = importlib.util.spec_from_file_location("debug_cleanup_test", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    monkeypatch.setattr(module, "verify_mcp_tools", action("verify", []))
    monkeypatch.setattr(
        threading, "Event", lambda: SimpleNamespace(is_set=lambda: True)
    )
    monkeypatch.setattr(signal, "signal", action("signal"))
    task = tmp_path / "task.yaml"
    task.write_text("task_id: T001\n", encoding="utf-8")
    args = SimpleNamespace(task=str(task), config=None, sandbox_image=None)
    return SimpleNamespace(
        run=lambda: module.run_debug(args),
        events=events,
        failures=failures,
        module=module,
        handle=handle,
        ctx=ctx,
    )


def cleanup_names(session):
    return [
        name
        for name, _, _ in session.events
        if name.startswith("cleanup_") or name == "stop_container"
    ]


@pytest.mark.parametrize(
    "stage", ["configure_tools", "reset_services", "start_mock_services", "verify"]
)
@pytest.mark.parametrize("error_type", [RuntimeError, KeyboardInterrupt])
def test_setup_failure_releases_acquired_resources(debug_session, stage, error_type):
    failure = error_type("setup failed")
    debug_session.failures[stage] = failure
    with pytest.raises(error_type) as raised:
        debug_session.run()
    assert raised.value is failure
    cleanup = cleanup_names(debug_session)
    expected = (
        ["stop_container"]
        if stage == "configure_tools"
        else ["cleanup_mock_services", "stop_container", "cleanup_config"]
    )
    if stage in {"start_mock_services", "verify"}:
        expected.append("cleanup_mock_services")
    assert cleanup == expected
    stop = next(event for event in debug_session.events if event[0] == "stop_container")
    assert stop[1] == (debug_session.handle,)
    configs = [event for event in debug_session.events if event[0] == "cleanup_config"]
    assert all(event[2] == {"context": debug_session.ctx} for event in configs)


def test_normal_exit_releases_resources_once_in_order(debug_session):
    debug_session.run()
    assert cleanup_names(debug_session) == [
        "cleanup_mock_services",
        "stop_container",
        "cleanup_config",
        "cleanup_mock_services",
    ]


@pytest.mark.parametrize("stage", ["start_container", "inject_files"])
def test_startup_failure_only_releases_an_acquired_container(debug_session, stage):
    debug_session.failures[stage] = RuntimeError("startup failed")
    with pytest.raises(SystemExit) as raised:
        debug_session.run()
    assert raised.value.code == 1
    assert cleanup_names(debug_session) == (
        [] if stage == "start_container" else ["stop_container"]
    )


@pytest.mark.parametrize("stage", ["stop_container", "cleanup_config"])
def test_cleanup_failure_does_not_skip_remaining_resources(debug_session, stage):
    debug_session.failures[stage] = RuntimeError("cleanup failed")
    debug_session.run()
    assert cleanup_names(debug_session) == [
        "cleanup_mock_services",
        "stop_container",
        "cleanup_config",
        "cleanup_mock_services",
    ]
    assert any(
        "cleanup failed" in str(args)
        for name, args, _ in debug_session.events
        if name == "log"
    )


def test_restart_timeout_uses_same_cleanup_order(debug_session):
    debug_session.module.restart_gateway = lambda *args: False
    with pytest.raises(SystemExit) as raised:
        debug_session.run()
    assert raised.value.code == 1
    assert cleanup_names(debug_session) == [
        "cleanup_mock_services",
        "stop_container",
        "cleanup_config",
        "cleanup_mock_services",
    ]


@pytest.mark.parametrize("stage", ["stop_container", "cleanup_config"])
def test_cleanup_interrupt_is_propagated_after_remaining_attempts(debug_session, stage):
    failure = KeyboardInterrupt("cleanup interrupted")
    debug_session.failures[stage] = failure
    with pytest.raises(KeyboardInterrupt) as raised:
        debug_session.run()
    assert raised.value is failure
    assert cleanup_names(debug_session) == [
        "cleanup_mock_services",
        "stop_container",
        "cleanup_config",
        "cleanup_mock_services",
    ]


@pytest.mark.parametrize("stage", ["start_container", "inject_files"])
def test_startup_interrupt_preserves_interrupt_and_owned_cleanup(debug_session, stage):
    failure = KeyboardInterrupt("startup interrupted")
    debug_session.failures[stage] = failure
    with pytest.raises(KeyboardInterrupt) as raised:
        debug_session.run()
    assert raised.value is failure
    assert cleanup_names(debug_session) == (
        [] if stage == "start_container" else ["stop_container"]
    )
