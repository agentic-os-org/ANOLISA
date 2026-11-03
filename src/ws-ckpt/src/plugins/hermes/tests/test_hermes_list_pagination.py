"""Contract tests for opt-in snapshot pagination through the Hermes tool."""

import json
import subprocess
from unittest.mock import MagicMock, patch

import hermes
import pytest
from hermes.tools import handle_ws_ckpt_list


@pytest.fixture
def workspace():
    with patch("hermes.tools._require_workspace", return_value=("/ws", None)):
        yield


def test_unpaged_contract(workspace):
    with patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, "complete table")) as run:
        assert json.loads(handle_ws_ckpt_list({})) == {"success": True, "output": "complete table"}
    run.assert_called_once_with(["ws-ckpt", "list", "-w", "/ws", "--format", "table"])


@pytest.mark.parametrize(
    "args,flags",
    [
        ({"limit": 1}, ["--limit", "1"]),
        ({"limit": 10000}, ["--limit", "10000"]),
        ({"cursor": " opaque cursor "}, ["--cursor", " opaque cursor "]),
        ({"limit": 2, "cursor": "next"}, ["--limit", "2", "--cursor", "next"]),
    ],
)
def test_one_cli_call_with_explicit_arguments(workspace, args, flags):
    page = {"snapshots": [], "next_cursor": None, "elapsed_secs": 0.01}
    with patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, json.dumps(page))) as run:
        raw = handle_ws_ckpt_list(args)
    assert isinstance(raw, str)
    assert json.loads(raw) == {"success": True, "snapshots": [], "next_cursor": None}
    run.assert_called_once_with(["ws-ckpt", "list", "-w", "/ws", "--format", "json"] + flags)


@pytest.mark.parametrize("value", [True, False, 1.0, 2.5, "2", None, 0, -1, 10001, [], {}])
def test_invalid_limit_never_invokes_cli(workspace, value):
    with patch("hermes.tools._run_ws_ckpt_cmd") as run:
        result = json.loads(handle_ws_ckpt_list({"limit": value}))
    assert result["success"] is False
    assert "limit" in result["error"]
    run.assert_not_called()


@pytest.mark.parametrize("value", [None, "", False, 1, 2.5, [], {}])
@pytest.mark.parametrize("with_limit", [False, True])
def test_invalid_cursor_never_invokes_cli(workspace, value, with_limit):
    args = {"cursor": value}
    if with_limit:
        args["limit"] = 2
    with patch("hermes.tools._run_ws_ckpt_cmd") as run:
        result = json.loads(handle_ws_ckpt_list(args))
    assert result["success"] is False
    assert "cursor" in result["error"]
    run.assert_not_called()


@pytest.mark.parametrize("snapshots", [[], [{"id": "only-one"}]])
def test_short_page_preserves_continuation(workspace, snapshots):
    page = {"snapshots": snapshots, "next_cursor": " still opaque "}
    with patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, json.dumps(page))) as run:
        assert json.loads(handle_ws_ckpt_list({"limit": 20})) == {"success": True, **page}
    run.assert_called_once()


def test_full_and_summary_objects_survive_recursively(workspace):
    snapshots = [
        {"id": "full", "meta": {"metadata": {"nested": [1, {"unknown": True}]}}},
        {
            "id": "summary",
            "detail": "summary",
            "omitted_fields": ["message", "metadata"],
            "meta": {"created_at": "2026-10-22T00:00:00Z", "pinned": False, "missing": False},
            "extension": {"v": [None, "保留"]},
        },
    ]
    page = {"snapshots": snapshots, "next_cursor": None, "future_field": {"x": 1}}
    with patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, json.dumps(page))):
        assert json.loads(handle_ws_ckpt_list({"limit": 2})) == {
            "success": True,
            "snapshots": snapshots,
            "next_cursor": None,
        }


@pytest.mark.parametrize(
    "output",
    [
        "not JSON",
        "[]",
        "null",
        "true",
        '"text"',
        "{}",
        '{"snapshots": []}',
        '{"next_cursor": null}',
        '{"snapshots": null, "next_cursor": null}',
        '{"snapshots": {}, "next_cursor": null}',
        '{"snapshots": [null], "next_cursor": null}',
        '{"snapshots": ["item"], "next_cursor": null}',
        '{"snapshots": [1], "next_cursor": null}',
        '{"snapshots": [[]], "next_cursor": null}',
        '{"snapshots": [], "next_cursor": ""}',
        '{"snapshots": [], "next_cursor": 3}',
        '{"snapshots": [], "next_cursor": false}',
    ],
)
def test_malformed_page_is_an_explicit_failure(workspace, output):
    with patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, output)) as run:
        result = json.loads(handle_ws_ckpt_list({"limit": 2}))
    assert result["success"] is False
    assert result["error"]
    assert "snapshots" not in result
    run.assert_called_once()


@pytest.mark.parametrize(
    "message",
    [
        "unexpected argument --limit",
        "invalid cursor",
        "cursor workspace mismatch",
        "daemon unavailable",
    ],
)
def test_cli_failure_is_preserved_without_retry(workspace, message):
    with patch("hermes.tools._run_ws_ckpt_cmd", return_value=(False, message)) as run:
        assert json.loads(handle_ws_ckpt_list({"limit": 2})) == {"success": False, "error": message}
    run.assert_called_once()


@pytest.mark.parametrize(
    "error,fragment",
    [
        (FileNotFoundError(), "not found"),
        (subprocess.TimeoutExpired("ws-ckpt", 30), "timed out"),
    ],
)
def test_process_failure_is_not_an_empty_page(workspace, error, fragment):
    with patch("hermes.tools.subprocess.run", side_effect=error) as run:
        result = json.loads(handle_ws_ckpt_list({"limit": 2}))
    assert result["success"] is False
    assert fragment in result["error"]
    run.assert_called_once()


def test_registered_tool_exposes_and_uses_pagination(workspace):
    ctx = MagicMock()
    hermes.register(ctx)
    tool = next(
        call.kwargs
        for call in ctx.register_tool.call_args_list
        if call.kwargs["name"] == "ws-ckpt-list"
    )
    parameters = tool["schema"]["parameters"]
    assert parameters.get("required", []) == []
    assert parameters["properties"]["limit"]["minimum"] == 1
    assert parameters["properties"]["limit"]["maximum"] == 10000
    assert parameters["properties"]["cursor"]["minLength"] == 1
    assert "next_cursor" in tool["schema"]["description"]
    assert "FULL untruncated" not in tool["schema"]["description"]
    snapshots = [{"id": str(i)} for i in range(5)]
    pages = [
        {"snapshots": snapshots[:2], "next_cursor": "page2"},
        {"snapshots": snapshots[2:4], "next_cursor": "page3"},
        {"snapshots": snapshots[4:], "next_cursor": None},
    ]
    with patch(
        "hermes.tools._run_ws_ckpt_cmd", side_effect=[(True, json.dumps(page)) for page in pages]
    ) as run:
        result = json.loads(tool["handler"]({"limit": 2}))
        collected = result["snapshots"][:]
        sizes = [len(collected)]
        while result["next_cursor"] is not None:
            result = json.loads(tool["handler"]({"limit": 2, "cursor": result["next_cursor"]}))
            collected.extend(result["snapshots"])
            sizes.append(len(result["snapshots"]))
    assert collected == snapshots
    assert sizes == [2, 2, 1]
    assert run.call_count == 3
    assert run.call_args_list[1].args[0][-2:] == ["--cursor", "page2"]
    assert run.call_args_list[2].args[0][-2:] == ["--cursor", "page3"]


@pytest.mark.parametrize("args", [{"limit": 2}, {"cursor": "next"}])
def test_pagination_requires_workspace(args):
    error = json.dumps({"success": False, "error": "No workspace configured"})
    with patch("hermes.tools._require_workspace", return_value=("", error)), patch(
        "hermes.tools._run_ws_ckpt_cmd"
    ) as run:
        assert handle_ws_ckpt_list(args) == error
    run.assert_not_called()
