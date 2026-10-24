"""Registered rollback and turn-end hooks; only CLI execution is mocked."""

import json
import subprocess
from types import SimpleNamespace
from pathlib import Path

import pytest

import hermes
from hermes import checkpoint_manager, tools
from hermes.checkpoint_manager import CheckpointManager, workspace_registration_path
from hermes.config import HermesPluginConfig


@pytest.fixture
def adapter(tmp_path, monkeypatch):
    default = tmp_path / "default"
    other = tmp_path / "other"
    default.mkdir()
    other.mkdir()
    manager = CheckpointManager(HermesPluginConfig(str(default), True))
    monkeypatch.setattr(checkpoint_manager, "_manager", manager)
    calls = []
    state = SimpleNamespace(fail=False, status_fail=False, status_payload=None)

    def run(argv, **kwargs):
        calls.append(argv)
        if argv[1] == "status" and "json" in argv:
            selector = argv[argv.index("--workspace") + 1]
            registered = other if selector == "ws-other" else default
            payload = state.status_payload
            if payload is None:
                payload = json.dumps(
                    {
                        "workspaces": [
                            {"ws_id": selector, "path": str(registered), "snapshot_count": 1}
                        ]
                    }
                )
            return subprocess.CompletedProcess(
                argv,
                int(state.status_fail),
                "" if state.status_fail else payload,
                "status unavailable" if state.status_fail else "",
            )
        failed = state.fail and argv[1] == "rollback"
        return subprocess.CompletedProcess(argv, int(failed), "done", "failed" if failed else "")

    monkeypatch.setattr(subprocess, "run", run)
    monkeypatch.setattr(tools, "_persist_plugin_yaml", lambda **kwargs: "")
    monkeypatch.setattr(hermes.CrontabManager, "migrate", lambda *args: [])
    hooks, registered = {}, {}
    hermes.register(SimpleNamespace(
        register_hook=lambda name, handler: hooks.__setitem__(name, handler),
        register_tool=lambda **kw: registered.__setitem__(kw["name"], kw["handler"]),
    ))

    def rollback(workspace=None, preview=False):
        args = {"target": "before-edit", "preview": preview}
        if workspace is not None:
            args["workspace"] = str(workspace)
        return json.loads(registered["ws-ckpt-rollback"](args))

    def configure(workspace):
        result = json.loads(registered["ws-ckpt-config"]({
            "action": "update", "key": "workspace", "value": str(workspace),
        }))
        assert result["success"]

    def checkpoints():
        return [argv[argv.index("--workspace") + 1] for argv in calls if argv[1] == "checkpoint"]

    return SimpleNamespace(
        default=default, other=other, state=state, calls=calls, manager=manager,
        rollback=rollback, configure=configure,
        end=lambda: hooks["on_session_end"](), checkpoints=checkpoints,
    )


def test_other_workspace_does_not_skip_default_checkpoint(adapter):
    assert adapter.rollback(adapter.other)["success"]
    assert adapter.calls[-1][3] == str(adapter.other)
    adapter.end()
    assert adapter.checkpoints() == [str(adapter.default)]


@pytest.mark.parametrize("explicit", [False, True])
def test_same_workspace_skips_exactly_one_turn(adapter, explicit):
    assert adapter.rollback(adapter.default if explicit else None)["success"]
    adapter.end()
    assert adapter.checkpoints() == []
    adapter.end()
    assert adapter.checkpoints() == [str(adapter.default)]


def test_configuration_change_does_not_transfer_or_retain_skip(adapter):
    assert adapter.rollback()["success"]
    adapter.configure(adapter.other)
    adapter.end()
    assert adapter.checkpoints() == [str(adapter.other)]
    adapter.configure(adapter.default)
    adapter.end()
    assert adapter.checkpoints() == [str(adapter.other), str(adapter.default)]


def test_configuration_can_switch_to_the_restored_workspace(adapter):
    assert adapter.rollback(adapter.other)["success"]
    adapter.configure(adapter.other)
    adapter.end()
    assert adapter.checkpoints() == []
    adapter.end()
    assert adapter.checkpoints() == [str(adapter.other)]


@pytest.mark.parametrize("preview,failed", [(True, False), (False, True)])
def test_preview_and_failure_do_not_skip(adapter, preview, failed):
    adapter.state.fail = failed
    assert adapter.rollback(preview=preview)["success"] is not failed
    adapter.end()
    assert adapter.checkpoints() == [str(adapter.default)]


def test_multiple_rollbacks_keep_each_workspaces_skip(adapter):
    assert adapter.rollback()["success"]
    assert adapter.rollback(adapter.other)["success"]
    adapter.end()
    assert adapter.checkpoints() == []
    adapter.end()
    assert adapter.checkpoints() == [str(adapter.default)]


def test_parent_alias_and_dot_spelling_match_registration(adapter, tmp_path):
    alias = tmp_path / "parent-alias"
    alias.symlink_to(tmp_path, target_is_directory=True)
    assert adapter.rollback(str(alias / "default") + "/./")["success"]
    adapter.end()
    assert adapter.checkpoints() == []


def test_parent_symlink_before_dotdot_does_not_match_lexically(adapter, tmp_path):
    nested = tmp_path / "nested"
    nested.mkdir()
    (nested / "child").mkdir()
    (nested / "default").mkdir()
    alias = tmp_path / "alias"
    alias.symlink_to(nested / "child", target_is_directory=True)
    assert adapter.rollback(str(alias) + "/../default")["success"]
    adapter.end()
    assert adapter.checkpoints() == [str(adapter.default)]


def test_final_symlink_target_change_preserves_registration_identity(adapter, tmp_path):
    anchor = tmp_path / "anchor"
    anchor.symlink_to(adapter.default, target_is_directory=True)
    adapter.configure(anchor)
    assert adapter.rollback()["success"]
    anchor.unlink()
    anchor.symlink_to(adapter.other, target_is_directory=True)
    adapter.end()
    assert adapter.checkpoints() == []


def test_distinct_registration_links_do_not_share_skip(adapter, tmp_path):
    anchor = tmp_path / "anchor"
    anchor.symlink_to(adapter.default, target_is_directory=True)
    assert adapter.rollback(anchor)["success"]
    adapter.end()
    assert adapter.checkpoints() == [str(adapter.default)]


def test_disabled_turn_consumes_suppression(adapter):
    assert adapter.rollback()["success"]
    adapter.manager.set_auto_checkpoint(False)
    adapter.end()
    adapter.manager.set_auto_checkpoint(True)
    adapter.end()
    assert adapter.checkpoints() == [str(adapter.default)]


@pytest.mark.parametrize("workspace,expected", [(".", str(Path.cwd().resolve())), ("/", "/")])
def test_registration_identity_for_current_directory_and_root(workspace, expected):
    assert workspace_registration_path(workspace) == expected


@pytest.mark.parametrize("state", ["enabled", "disabled", "unavailable_cli"])
def test_turn_without_pending_rollback_does_not_resolve_registration(
    adapter: SimpleNamespace, monkeypatch: pytest.MonkeyPatch, state: str
) -> None:
    if state == "disabled":
        adapter.manager.set_auto_checkpoint(False)
    elif state == "unavailable_cli":

        def unavailable(*args: object, **kwargs: object) -> None:
            raise FileNotFoundError("ws-ckpt is unavailable")

        monkeypatch.setattr(subprocess, "run", unavailable)

    strict_resolutions = []
    original_resolve = Path.resolve

    def track_resolve(path: Path, strict: bool = False) -> Path:
        if strict:
            strict_resolutions.append(path)
        return original_resolve(path, strict=strict)

    with monkeypatch.context() as patch:
        patch.setattr(Path, "resolve", track_resolve)
        adapter.end()

    assert strict_resolutions == []
    assert adapter.checkpoints() == ([str(adapter.default)] if state == "enabled" else [])


@pytest.mark.parametrize("state", ["disabled", "unavailable_cli"])
def test_unready_turn_consumes_other_workspace_suppression(
    adapter: SimpleNamespace, monkeypatch: pytest.MonkeyPatch, state: str
) -> None:
    assert adapter.rollback(adapter.other)["success"]
    with monkeypatch.context() as patch:
        if state == "disabled":
            adapter.manager.set_auto_checkpoint(False)
        else:

            def unavailable(*args: object, **kwargs: object) -> None:
                raise FileNotFoundError("ws-ckpt is unavailable")

            patch.setattr(subprocess, "run", unavailable)
        adapter.end()

    assert adapter.manager.skip_auto_checkpoint_workspaces == set()
    adapter.manager.set_auto_checkpoint(True)
    adapter.configure(adapter.other)
    adapter.end()
    assert adapter.checkpoints() == [str(adapter.other)]


def test_workspace_id_suppresses_its_registered_path_once(adapter: SimpleNamespace) -> None:
    assert adapter.rollback("ws-default")["success"]
    rollback = next(argv for argv in adapter.calls if argv[1] == "rollback")
    assert rollback[3] == "ws-default"
    adapter.end()
    assert adapter.checkpoints() == []
    adapter.end()
    assert adapter.checkpoints() == [str(adapter.default)]


def test_other_workspace_id_does_not_suppress_default(adapter: SimpleNamespace) -> None:
    assert adapter.rollback("ws-other")["success"]
    adapter.end()
    assert adapter.checkpoints() == [str(adapter.default)]


def test_workspace_id_matches_after_configuration_switch(adapter: SimpleNamespace) -> None:
    assert adapter.rollback("ws-other")["success"]
    adapter.configure(adapter.other)
    adapter.end()
    assert adapter.checkpoints() == []


@pytest.mark.parametrize("explicit", [True, False])
def test_configured_workspace_id_matches_rollback_path(
    adapter: SimpleNamespace, explicit: bool
) -> None:
    adapter.configure("ws-default")
    assert adapter.rollback(adapter.default if explicit else None)["success"]
    adapter.end()
    assert adapter.checkpoints() == []
    adapter.end()
    assert adapter.checkpoints() == ["ws-default"]


@pytest.mark.parametrize("selector", ["ws-default", "project"])
def test_bare_selector_uses_daemon_registration_over_local_directory(
    adapter: SimpleNamespace, monkeypatch: pytest.MonkeyPatch, selector: str
) -> None:
    (adapter.default.parent / selector).mkdir()
    monkeypatch.chdir(adapter.default.parent)
    assert adapter.rollback(selector)["success"]
    adapter.end()
    assert adapter.checkpoints() == []


@pytest.mark.parametrize(
    "payload",
    [
        "not json",
        "null",
        "{}",
        '{"workspaces":[]}',
        '{"workspaces":[{},{}]}',
        '{"workspaces":[{"path":"relative"}]}',
        '{"workspaces":[{"path":""}]}',
    ],
)
def test_bad_registration_response_prevents_rollback(
    adapter: SimpleNamespace, payload: str
) -> None:
    adapter.state.status_payload = payload
    result = adapter.rollback("ws-default")
    assert not result["success"]
    assert "resolve workspace" in result["error"]
    assert not any(argv[1] == "rollback" for argv in adapter.calls)
    adapter.end()
    assert adapter.checkpoints() == [str(adapter.default)]


def test_registration_lookup_failure_prevents_rollback(adapter: SimpleNamespace) -> None:
    adapter.state.status_fail = True
    result = adapter.rollback("ws-default")
    assert not result["success"]
    assert "status unavailable" in result["error"]
    assert not any(argv[1] == "rollback" for argv in adapter.calls)


def test_id_preview_does_not_query_or_suppress(adapter: SimpleNamespace) -> None:
    adapter.state.status_fail = True
    assert adapter.rollback("ws-default", preview=True)["success"]
    assert not any(argv[1] == "status" for argv in adapter.calls)
    adapter.end()
    assert adapter.checkpoints() == [str(adapter.default)]


def test_failed_id_rollback_does_not_suppress(adapter: SimpleNamespace) -> None:
    adapter.state.fail = True
    assert not adapter.rollback("ws-default")["success"]
    adapter.end()
    assert adapter.checkpoints() == [str(adapter.default)]


def test_hook_lookup_failure_is_visible_and_consumed(
    adapter: SimpleNamespace, capsys: pytest.CaptureFixture[str]
) -> None:
    adapter.configure("ws-default")
    assert adapter.rollback(adapter.default)["success"]
    adapter.state.status_fail = True
    adapter.end()
    assert "status unavailable" in capsys.readouterr().out
    assert adapter.checkpoints() == ["ws-default"]
    adapter.state.status_fail = False
    adapter.end()
    assert adapter.checkpoints() == ["ws-default", "ws-default"]


def test_path_rollback_adds_no_status_lookup(adapter: SimpleNamespace) -> None:
    assert adapter.rollback(adapter.default)["success"]
    adapter.end()
    assert not any(argv[1] == "status" for argv in adapter.calls)
