"""Tests for hermes plugin tools module.

Covers pure functions (_parse_workspace_policy_json, _render_workspace_policy,
_json/_ok/_err, check_ws_ckpt_available, map helpers) and handler dispatch
with subprocess mocked out.
"""

import json
from unittest.mock import MagicMock, patch

import pytest

from hermes.checkpoint_manager import get_manager
from hermes.tools import (
    _parse_workspace_policy_json,
    _render_workspace_policy,
    _json,
    _ok,
    _err,
    _run_ws_ckpt_cmd,
    _resolve_workspace,
    _require_workspace,
    _reject_if_cwd_inside_workspace,
    _persist_plugin_yaml,
    check_ws_ckpt_available,
    handle_ws_ckpt_config,
    handle_ws_ckpt_checkpoint,
    handle_ws_ckpt_rollback,
    handle_ws_ckpt_list,
    handle_ws_ckpt_diff,
    handle_ws_ckpt_delete,
    handle_ws_ckpt_status,
    TOOLS,
)


# ---------------------------------------------------------------------------
# JSON helpers
# ---------------------------------------------------------------------------


class TestJsonHelpers:
    def test_json(self):
        assert json.loads(_json({"a": 1})) == {"a": 1}

    def test_ok(self):
        result = json.loads(_ok("done"))
        assert result["success"] is True
        assert result["output"] == "done"

    def test_err(self):
        result = json.loads(_err("bad"))
        assert result["success"] is False
        assert result["error"] == "bad"


# ---------------------------------------------------------------------------
# _parse_workspace_policy_json
# ---------------------------------------------------------------------------


class TestParseWorkspacePolicyJson:
    def _v1_doc(self, effective):
        return json.dumps({"schema": "ws-ckpt-policy/v1", "effective": effective})

    def test_invalid_json(self):
        r = _parse_workspace_policy_json("not json")
        assert r["kind"] == "parse-error"

    def test_non_object_root(self):
        r = _parse_workspace_policy_json('"hello"')
        assert r["kind"] == "parse-error"

    def test_wrong_schema(self):
        r = _parse_workspace_policy_json(json.dumps({"schema": "v99"}))
        assert r["kind"] == "parse-error"
        assert "v99" in r["reason"]

    def test_missing_effective(self):
        r = _parse_workspace_policy_json(json.dumps({"schema": "ws-ckpt-policy/v1"}))
        assert r["kind"] == "parse-error"

    def test_disabled(self):
        r = _parse_workspace_policy_json(self._v1_doc({"is_disabled": True}))
        assert r["kind"] == "disabled"

    def test_count_mode(self):
        r = _parse_workspace_policy_json(self._v1_doc({
            "auto_cleanup_keep": {"mode": "count", "count": 5}
        }))
        assert r == {"kind": "count", "num": 5}

    def test_count_zero(self):
        r = _parse_workspace_policy_json(self._v1_doc({
            "auto_cleanup_keep": {"mode": "count", "count": 0}
        }))
        assert r == {"kind": "count", "num": 0}

    def test_count_bool_rejected(self):
        r = _parse_workspace_policy_json(self._v1_doc({
            "auto_cleanup_keep": {"mode": "count", "count": True}
        }))
        assert r["kind"] == "parse-error"

    def test_count_negative_rejected(self):
        r = _parse_workspace_policy_json(self._v1_doc({
            "auto_cleanup_keep": {"mode": "count", "count": -1}
        }))
        assert r["kind"] == "parse-error"

    def test_count_float_rejected(self):
        r = _parse_workspace_policy_json(self._v1_doc({
            "auto_cleanup_keep": {"mode": "count", "count": 3.5}
        }))
        assert r["kind"] == "parse-error"

    def test_age_mode(self):
        r = _parse_workspace_policy_json(self._v1_doc({
            "auto_cleanup_keep": {"mode": "age", "raw": "7d"}
        }))
        assert r == {"kind": "age", "duration": "7d"}

    def test_age_raw_not_string(self):
        r = _parse_workspace_policy_json(self._v1_doc({
            "auto_cleanup_keep": {"mode": "age", "raw": 42}
        }))
        assert r["kind"] == "parse-error"

    def test_unknown_mode(self):
        r = _parse_workspace_policy_json(self._v1_doc({
            "auto_cleanup_keep": {"mode": "fancy"}
        }))
        assert r["kind"] == "parse-error"

    def test_missing_auto_cleanup_keep(self):
        r = _parse_workspace_policy_json(self._v1_doc({"is_disabled": False}))
        assert r["kind"] == "parse-error"


# ---------------------------------------------------------------------------
# _render_workspace_policy
# ---------------------------------------------------------------------------


class TestRenderWorkspacePolicy:
    def _v1(self, effective):
        return json.dumps({"schema": "ws-ckpt-policy/v1", "effective": effective})

    def test_disabled(self):
        lines = _render_workspace_policy(self._v1({"is_disabled": True}), "/ws")
        text = "\n".join(lines)
        assert "disabled" in text

    def test_count(self):
        lines = _render_workspace_policy(
            self._v1({"auto_cleanup_keep": {"mode": "count", "count": 10}}), "/ws"
        )
        text = "\n".join(lines)
        assert "10" in text

    def test_age(self):
        lines = _render_workspace_policy(
            self._v1({"auto_cleanup_keep": {"mode": "age", "raw": "24h"}}), "/ws"
        )
        text = "\n".join(lines)
        assert "24h" in text

    def test_parse_error(self):
        lines = _render_workspace_policy("not json", "/ws")
        text = "\n".join(lines)
        assert "schema" in text.lower() or "not valid JSON" in text


# ---------------------------------------------------------------------------
# _run_ws_ckpt_cmd
# ---------------------------------------------------------------------------


class TestRunWsCkptCmd:
    @patch("hermes.tools.subprocess.run")
    def test_success(self, mock_run):
        mock_run.return_value = MagicMock(returncode=0, stdout="ok\n", stderr="")
        ok, output = _run_ws_ckpt_cmd(["ws-ckpt", "status"])
        assert ok is True
        assert output == "ok"

    @patch("hermes.tools.subprocess.run")
    def test_failure(self, mock_run):
        mock_run.return_value = MagicMock(returncode=1, stdout="", stderr="err msg")
        ok, output = _run_ws_ckpt_cmd(["ws-ckpt", "status"])
        assert ok is False
        assert output == "err msg"

    @patch("hermes.tools.subprocess.run")
    def test_timeout(self, mock_run):
        import subprocess
        mock_run.side_effect = subprocess.TimeoutExpired(cmd="ws-ckpt", timeout=240)
        ok, output = _run_ws_ckpt_cmd(["ws-ckpt", "status"])
        assert ok is False
        assert "timed out" in output.lower()

    @patch("hermes.tools.subprocess.run")
    def test_file_not_found(self, mock_run):
        mock_run.side_effect = FileNotFoundError()
        ok, output = _run_ws_ckpt_cmd(["ws-ckpt", "status"])
        assert ok is False
        assert "not found" in output.lower()

    @patch("hermes.tools.subprocess.run")
    def test_generic_exception(self, mock_run):
        mock_run.side_effect = RuntimeError("boom")
        ok, output = _run_ws_ckpt_cmd(["ws-ckpt", "status"])
        assert ok is False
        assert "boom" in output


# ---------------------------------------------------------------------------
# check_ws_ckpt_available
# ---------------------------------------------------------------------------


class TestCheckWsCkptAvailable:
    def test_caches_result(self):
        import hermes.tools as t
        original = t._ws_ckpt_available
        try:
            t._ws_ckpt_available = None
            with patch("hermes.tools.shutil.which", return_value="/usr/bin/ws-ckpt"):
                assert check_ws_ckpt_available() is True
            assert t._ws_ckpt_available is True
            # Second call should use cache, not call which again
            with patch("hermes.tools.shutil.which", return_value=None):
                assert check_ws_ckpt_available() is True
        finally:
            t._ws_ckpt_available = original

    def test_not_available(self):
        import hermes.tools as t
        original = t._ws_ckpt_available
        try:
            t._ws_ckpt_available = None
            with patch("hermes.tools.shutil.which", return_value=None):
                assert check_ws_ckpt_available() is False
        finally:
            t._ws_ckpt_available = original


# ---------------------------------------------------------------------------
# Handlers (with _require_workspace mocked)
# ---------------------------------------------------------------------------


class TestHandlers:
    @patch("hermes.tools._require_workspace", return_value=("", _err("No workspace")))
    def test_checkpoint_no_workspace(self, _):
        result = json.loads(handle_ws_ckpt_checkpoint({}))
        assert result["success"] is False

    @patch("hermes.tools._require_workspace", return_value=("", _err("No workspace")))
    def test_list_no_workspace(self, _):
        result = json.loads(handle_ws_ckpt_list({}))
        assert result["success"] is False

    @patch("hermes.tools._require_workspace", return_value=("", _err("No workspace")))
    def test_status_no_workspace(self, _):
        result = json.loads(handle_ws_ckpt_status({}))
        assert result["success"] is False

    def test_diff_missing_from(self):
        with patch("hermes.tools._require_workspace", return_value=("/ws", None)):
            result = json.loads(handle_ws_ckpt_diff({"to": "b"}))
        assert result["success"] is False

    def test_diff_missing_to(self):
        with patch("hermes.tools._require_workspace", return_value=("/ws", None)):
            result = json.loads(handle_ws_ckpt_diff({"from": "a"}))
        assert result["success"] is False

    def test_delete_missing_snapshot(self):
        result = json.loads(handle_ws_ckpt_delete({}))
        assert result["success"] is False

    def test_checkpoint_missing_id(self):
        with patch("hermes.tools._resolve_workspace", return_value=("/ws", None)):
            with patch("hermes.tools._reject_if_cwd_inside_workspace", return_value=None):
                result = json.loads(handle_ws_ckpt_checkpoint({}))
        assert result["success"] is False

    def test_rollback_missing_target(self):
        result = json.loads(handle_ws_ckpt_rollback({}))
        assert result["success"] is False

    def test_rollback_both_target_and_num_ancestors(self):
        result = json.loads(handle_ws_ckpt_rollback({"target": "snap1", "num_ancestors": 2}))
        assert result["success"] is False
        assert "mutually exclusive" in result["error"]

    def test_rollback_num_ancestors_invalid(self):
        result = json.loads(handle_ws_ckpt_rollback({"num_ancestors": "abc"}))
        assert result["success"] is False
        assert "integer" in result["error"]

    def test_rollback_num_ancestors_zero(self):
        result = json.loads(handle_ws_ckpt_rollback({"num_ancestors": 0}))
        assert result["success"] is False
        assert ">= 1" in result["error"]

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, "rolled back"))
    @patch("hermes.tools._reject_if_cwd_inside_workspace", return_value=None)
    @patch("hermes.tools._resolve_workspace", return_value=("/ws", None))
    def test_rollback_num_ancestors_success(self, _ws, _cwd, mock_cmd):
        result = json.loads(handle_ws_ckpt_rollback({"num_ancestors": 2}))
        assert result["success"] is True
        cmd_args = mock_cmd.call_args[0][0]
        assert "-n" in cmd_args
        assert "2" in cmd_args

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, "rolled back"))
    @patch("hermes.tools._reject_if_cwd_inside_workspace", return_value=None)
    @patch("hermes.tools._resolve_workspace", return_value=("/ws", None))
    def test_rollback_sets_skip_flag(self, _ws, _cwd, _cmd):
        mgr = get_manager()
        mgr.skip_next_auto_checkpoint = False
        handle_ws_ckpt_rollback({"target": "snap1"})
        assert mgr.skip_next_auto_checkpoint is True

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, "preview"))
    @patch("hermes.tools._reject_if_cwd_inside_workspace")
    @patch("hermes.tools._resolve_workspace", return_value=("/ws", None))
    def test_rollback_preview_does_not_set_skip_flag(self, _ws, _cwd, _cmd):
        mgr = get_manager()
        mgr.skip_next_auto_checkpoint = False
        handle_ws_ckpt_rollback({"target": "snap1", "preview": True})
        assert mgr.skip_next_auto_checkpoint is False

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(False, "failed"))
    @patch("hermes.tools._reject_if_cwd_inside_workspace", return_value=None)
    @patch("hermes.tools._resolve_workspace", return_value=("/ws", None))
    def test_rollback_failure_does_not_set_skip_flag(self, _ws, _cwd, _cmd):
        mgr = get_manager()
        mgr.skip_next_auto_checkpoint = False
        handle_ws_ckpt_rollback({"target": "snap1"})
        assert mgr.skip_next_auto_checkpoint is False

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, "Rollback preview\nM  file.txt"))
    @patch("hermes.tools._reject_if_cwd_inside_workspace")
    @patch("hermes.tools._resolve_workspace", return_value=("/ws", None))
    def test_rollback_preview(self, _ws, mock_cwd, mock_cmd):
        result = json.loads(handle_ws_ckpt_rollback({"target": "snap1", "preview": True}))
        assert result["success"] is True
        assert "M  file.txt" in result["output"]
        assert "--preview" in mock_cmd.call_args[0][0]
        mock_cwd.assert_not_called()

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, "snap1 created"))
    @patch("hermes.tools._reject_if_cwd_inside_workspace", return_value=None)
    @patch("hermes.tools._resolve_workspace", return_value=("/ws", None))
    def test_checkpoint_success(self, _ws, _cwd, _cmd):
        result = json.loads(handle_ws_ckpt_checkpoint({"id": "snap1"}))
        assert result["success"] is True

    @patch("hermes.tools.subprocess.run")
    @patch("hermes.tools._reject_if_cwd_inside_workspace", return_value=None)
    @patch("hermes.tools._resolve_workspace", return_value=("/ws", None))
    def test_rollback_uses_cleanup_compatible_timeout(self, _ws, _cwd, mock_run):
        mock_run.return_value = MagicMock(returncode=0, stdout="rolled back", stderr="")
        result = json.loads(handle_ws_ckpt_rollback({"target": "snap1"}))
        assert result["success"] is True
        assert mock_run.call_args.kwargs["timeout"] == 240

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, "ID  MESSAGE"))
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    def test_list_success(self, _ws, _cmd):
        result = json.loads(handle_ws_ckpt_list({}))
        assert result["success"] is True

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, "diff output"))
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    def test_diff_success(self, _ws, _cmd):
        result = json.loads(handle_ws_ckpt_diff({"from": "a", "to": "b"}))
        assert result["success"] is True

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, "deleted"))
    @patch("hermes.tools._resolve_workspace", return_value=("/ws", None))
    def test_delete_success(self, _ws, _cmd):
        result = json.loads(handle_ws_ckpt_delete({"snapshot": "snap1"}))
        assert result["success"] is True

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, "running"))
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    def test_status_success(self, _ws, _cmd):
        result = json.loads(handle_ws_ckpt_status({}))
        assert result["success"] is True


# ---------------------------------------------------------------------------
# TOOLS tuple
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# _resolve_workspace / _require_workspace / _reject_if_cwd_inside_workspace
# ---------------------------------------------------------------------------


class TestResolveWorkspace:
    @patch("hermes.tools.get_manager")
    def test_explicit_workspace(self, mock_mgr):
        mock_mgr.return_value.config.workspace = "/ws"
        ws, err = _resolve_workspace({"workspace": "/explicit"})
        assert ws == "/explicit"
        assert err is None

    @patch("hermes.tools.get_manager")
    def test_fallback_to_default(self, mock_mgr):
        mock_mgr.return_value.config.workspace = "/ws"
        ws, err = _resolve_workspace({})
        assert ws == "/ws"
        assert err is None

    @patch("hermes.tools.get_manager")
    def test_no_workspace(self, mock_mgr):
        mock_mgr.return_value.config.workspace = ""
        ws, err = _resolve_workspace({})
        assert ws == ""
        assert err is not None


class TestRejectIfCwdInsideWorkspace:
    @patch("hermes.tools.cwd_inside_workspace", return_value=(True, "/ws/sub"))
    def test_inside(self, _):
        result = _reject_if_cwd_inside_workspace("/ws")
        assert result is not None
        parsed = json.loads(result)
        assert parsed["success"] is False

    @patch("hermes.tools.cwd_inside_workspace", return_value=(False, "/other"))
    def test_outside(self, _):
        result = _reject_if_cwd_inside_workspace("/ws")
        assert result is None


# ---------------------------------------------------------------------------
# handle_ws_ckpt_config
# ---------------------------------------------------------------------------


class TestHandleWsCkptConfig:
    # --- view ---
    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, '{"schema":"ws-ckpt-policy/v1","effective":{"is_disabled":true}}'))
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    @patch("hermes.tools.load_config")
    def test_view_success(self, mock_cfg, _ws, _cmd):
        from hermes.config import HermesPluginConfig
        mock_cfg.return_value = HermesPluginConfig(workspace="/ws", auto_checkpoint=False)
        result = json.loads(handle_ws_ckpt_config({}))
        assert result["success"] is True
        assert "autoCheckpoint" in result["output"]

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(False, "daemon down"))
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    @patch("hermes.tools.load_config")
    def test_view_daemon_failure(self, mock_cfg, _ws, _cmd):
        from hermes.config import HermesPluginConfig
        mock_cfg.return_value = HermesPluginConfig(workspace="/ws", auto_checkpoint=False)
        result = json.loads(handle_ws_ckpt_config({"action": "view"}))
        assert result["success"] is True
        assert "failed to query" in result["output"]

    @patch("hermes.tools._require_workspace", return_value=("", _err("No workspace")))
    def test_view_no_workspace(self, _):
        result = json.loads(handle_ws_ckpt_config({"action": "view"}))
        assert result["success"] is False

    # --- unknown action ---
    def test_unknown_action(self):
        result = json.loads(handle_ws_ckpt_config({"action": "destroy"}))
        assert result["success"] is False
        assert "Unknown action" in result["error"]

    # --- update without key ---
    def test_update_no_key(self):
        result = json.loads(handle_ws_ckpt_config({"action": "update"}))
        assert result["success"] is False
        assert "Usage" in result["error"]

    # --- update unknown key ---
    def test_update_unknown_key(self):
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "badKey"}))
        assert result["success"] is False
        assert "Unknown config key" in result["error"]

    # --- maxSnapshotsNum ---
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    def test_maxSnapshotsNum_no_value(self, _):
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "maxSnapshotsNum"}))
        assert result["success"] is False
        assert "requires a value" in result["error"]

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, "reset"))
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    def test_maxSnapshotsNum_unset(self, _ws, _cmd):
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "maxSnapshotsNum", "value": "unset"}))
        assert result["success"] is True
        assert "unset" in result["output"]

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(False, "fail"))
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    def test_maxSnapshotsNum_unset_failure(self, _ws, _cmd):
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "maxSnapshotsNum", "value": "unset"}))
        assert result["success"] is False

    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    def test_maxSnapshotsNum_invalid(self, _):
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "maxSnapshotsNum", "value": "abc"}))
        assert result["success"] is False
        assert "positive integer" in result["error"]

    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    def test_maxSnapshotsNum_zero(self, _):
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "maxSnapshotsNum", "value": "0"}))
        assert result["success"] is False

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, "ok"))
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    def test_maxSnapshotsNum_success(self, _ws, _cmd):
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "maxSnapshotsNum", "value": "10"}))
        assert result["success"] is True
        assert "10" in result["output"]

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(False, "error"))
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    def test_maxSnapshotsNum_cmd_failure(self, _ws, _cmd):
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "maxSnapshotsNum", "value": "10"}))
        assert result["success"] is False

    # --- maxSnapshotsDuration ---
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    def test_maxSnapshotsDuration_no_value(self, _):
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "maxSnapshotsDuration"}))
        assert result["success"] is False

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, "ok"))
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    def test_maxSnapshotsDuration_success(self, _ws, _cmd):
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "maxSnapshotsDuration", "value": "7d"}))
        assert result["success"] is True
        assert "7d" in result["output"]

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, "reset"))
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    def test_maxSnapshotsDuration_unset(self, _ws, _cmd):
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "maxSnapshotsDuration", "value": "unset"}))
        assert result["success"] is True

    @patch("hermes.tools._require_workspace", return_value=("", _err("No workspace")))
    def test_maxSnapshotsNum_no_workspace(self, _):
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "maxSnapshotsNum", "value": "5"}))
        assert result["success"] is False

    # --- autoCheckpoint ---
    def test_autoCheckpoint_no_value(self):
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "autoCheckpoint"}))
        assert result["success"] is False

    def test_autoCheckpoint_invalid_value(self):
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "autoCheckpoint", "value": "maybe"}))
        assert result["success"] is False

    @patch("hermes.tools._persist_plugin_yaml", return_value="")
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    @patch("hermes.tools._reject_if_cwd_inside_workspace", return_value=None)
    @patch("hermes.tools.get_manager")
    def test_autoCheckpoint_true(self, mock_mgr, _cwd, _ws, _persist):
        mock_mgr.return_value = MagicMock()
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "autoCheckpoint", "value": "true"}))
        assert result["success"] is True

    @patch("hermes.tools._persist_plugin_yaml", return_value="")
    @patch("hermes.tools.get_manager")
    def test_autoCheckpoint_false(self, mock_mgr, _persist):
        mock_mgr.return_value = MagicMock()
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "autoCheckpoint", "value": "false"}))
        assert result["success"] is True

    @patch("hermes.tools._persist_plugin_yaml", return_value="save error")
    @patch("hermes.tools.get_manager")
    def test_autoCheckpoint_persist_failure(self, mock_mgr, _persist):
        mock_mgr.return_value = MagicMock()
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "autoCheckpoint", "value": "false"}))
        assert result["success"] is False
        assert "persist" in result["error"].lower()

    @pytest.mark.parametrize("val", ["1", "yes", "on", "enabled"])
    @patch("hermes.tools._persist_plugin_yaml", return_value="")
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    @patch("hermes.tools._reject_if_cwd_inside_workspace", return_value=None)
    @patch("hermes.tools.get_manager")
    def test_autoCheckpoint_truthy_aliases(self, mock_mgr, _cwd, _ws, _persist, val):
        mock_mgr.return_value = MagicMock()
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "autoCheckpoint", "value": val}))
        assert result["success"] is True

    @pytest.mark.parametrize("val", ["0", "no", "off", "disabled"])
    @patch("hermes.tools._persist_plugin_yaml", return_value="")
    @patch("hermes.tools.get_manager")
    def test_autoCheckpoint_falsy_aliases(self, mock_mgr, _persist, val):
        mock_mgr.return_value = MagicMock()
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "autoCheckpoint", "value": val}))
        assert result["success"] is True

    # --- workspace ---
    def test_workspace_no_value(self):
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "workspace"}))
        assert result["success"] is False

    @patch("hermes.tools._persist_plugin_yaml", return_value="")
    @patch("hermes.tools.get_manager")
    def test_workspace_success(self, mock_mgr, _persist):
        mock_mgr.return_value = MagicMock()
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "workspace", "value": "/new"}))
        assert result["success"] is True
        assert "/new" in result["output"]

    @patch("hermes.tools._persist_plugin_yaml", return_value="error")
    @patch("hermes.tools.get_manager")
    def test_workspace_persist_failure(self, mock_mgr, _persist):
        mock_mgr.return_value = MagicMock()
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "workspace", "value": "/new"}))
        assert result["success"] is False

    # --- cronSchedules ---
    def test_cronSchedules_no_value(self):
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "cronSchedules"}))
        assert result["success"] is False
        assert "requires a value" in result["error"]

    @patch("hermes.tools.get_manager")
    def test_cronSchedules_no_workspace(self, mock_mgr):
        mock_mgr.return_value.config.workspace = ""
        mock_mgr.return_value.config.cron_schedules = []
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "cronSchedules", "value": 'add "0 * * * *"'}))
        assert result["success"] is False

    @patch("hermes.cron.CrontabManager.sync_with_retry", return_value=True)
    @patch("hermes.tools._persist_plugin_yaml", return_value="")
    @patch("hermes.tools.get_manager")
    def test_cronSchedules_add_success(self, mock_mgr, _persist, _cron):
        mock_mgr.return_value.config.workspace = "/ws"
        mock_mgr.return_value.config.cron_schedules = []
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "cronSchedules", "value": 'add "0 * * * *"'}))
        assert result["success"] is True
        assert "0 * * * *" in result["output"]

    @patch("hermes.tools.get_manager")
    def test_cronSchedules_add_invalid(self, mock_mgr):
        mock_mgr.return_value.config.workspace = "/ws"
        mock_mgr.return_value.config.cron_schedules = []
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "cronSchedules", "value": "add bad"}))
        assert result["success"] is False
        assert "Invalid cron" in result["error"]

    @patch("hermes.cron.CrontabManager.sync_with_retry", return_value=True)
    @patch("hermes.tools._persist_plugin_yaml", return_value="")
    @patch("hermes.tools.get_manager")
    def test_cronSchedules_remove_success(self, mock_mgr, _persist, _cron):
        mock_mgr.return_value.config.workspace = "/ws"
        mock_mgr.return_value.config.cron_schedules = ["0 * * * *"]
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "cronSchedules", "value": "remove 0 * * * *"}))
        assert result["success"] is True

    @patch("hermes.cron.CrontabManager.sync_with_retry", return_value=True)
    @patch("hermes.tools._persist_plugin_yaml", return_value="")
    @patch("hermes.tools.get_manager")
    def test_cronSchedules_set_success(self, mock_mgr, _persist, _cron):
        mock_mgr.return_value.config.workspace = "/ws"
        mock_mgr.return_value.config.cron_schedules = []
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "cronSchedules", "value": 'set ["0 * * * *"]'}))
        assert result["success"] is True

    @patch("hermes.cron.CrontabManager.sync_with_retry", return_value=False)
    @patch("hermes.tools._persist_plugin_yaml", return_value="")
    @patch("hermes.tools.get_manager")
    def test_cronSchedules_sync_failure(self, mock_mgr, _persist, _cron):
        mock_mgr.return_value.config.workspace = "/ws"
        mock_mgr.return_value.config.cron_schedules = []
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "cronSchedules", "value": 'add "0 * * * *"'}))
        assert result["success"] is True
        assert "WARNING" in result["output"]

    @patch("hermes.tools._persist_plugin_yaml", return_value="save error")
    @patch("hermes.tools.get_manager")
    def test_cronSchedules_persist_failure(self, mock_mgr, _persist):
        mock_mgr.return_value.config.workspace = "/ws"
        mock_mgr.return_value.config.cron_schedules = []
        result = json.loads(handle_ws_ckpt_config({"action": "update", "key": "cronSchedules", "value": 'add "0 * * * *"'}))
        assert result["success"] is False
        assert "persist" in result["error"].lower()

    # --- view with cron_schedules ---
    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, '{"schema":"ws-ckpt-policy/v1","effective":{"is_disabled":true}}'))
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    @patch("hermes.tools.load_config")
    def test_view_with_cron_schedules(self, mock_cfg, _ws, _cmd):
        from hermes.config import HermesPluginConfig
        mock_cfg.return_value = HermesPluginConfig(workspace="/ws", auto_checkpoint=True, cron_schedules=["0 * * * *"])
        result = json.loads(handle_ws_ckpt_config({}))
        assert result["success"] is True
        assert "0 * * * *" in result["output"]

    # --- "set" alias for "update" ---
    def test_set_alias(self):
        result = json.loads(handle_ws_ckpt_config({"action": "set", "key": "workspace"}))
        assert result["success"] is False
        assert "requires" in result["error"]


# ---------------------------------------------------------------------------
# _persist_plugin_yaml
# ---------------------------------------------------------------------------


class TestPersistPluginYaml:
    @patch("hermes.tools._persist_plugin_yaml.__module__", "hermes.tools")
    def test_hermes_cli_not_available(self):
        with patch.dict("sys.modules", {"hermes_cli": None, "hermes_cli.config": None}):
            err = _persist_plugin_yaml(autoCheckpoint=True)
        assert "hermes_cli" in err.lower() or "not available" in err.lower()

    def test_import_failure(self):
        import sys
        saved = sys.modules.get("hermes_cli.config")
        try:
            sys.modules["hermes_cli.config"] = None
            err = _persist_plugin_yaml(autoCheckpoint=True)
            assert err != ""
        finally:
            if saved is not None:
                sys.modules["hermes_cli.config"] = saved
            else:
                sys.modules.pop("hermes_cli.config", None)

    @patch("hermes.tools._persist_plugin_yaml.__module__", "hermes.tools")
    def test_is_managed_returns_error(self):
        mock_cfg_mod = MagicMock()
        mock_cfg_mod.is_managed.return_value = True
        with patch.dict("sys.modules", {"hermes_cli": MagicMock(), "hermes_cli.config": mock_cfg_mod}):
            err = _persist_plugin_yaml(autoCheckpoint=True)
        assert "managed" in err.lower()

    @patch("hermes.tools._persist_plugin_yaml.__module__", "hermes.tools")
    def test_success(self):
        mock_cfg_mod = MagicMock()
        mock_cfg_mod.is_managed.return_value = False
        mock_cfg_mod.load_config.return_value = {"plugins": {"ws-ckpt": {}}}
        with patch.dict("sys.modules", {"hermes_cli": MagicMock(), "hermes_cli.config": mock_cfg_mod}):
            err = _persist_plugin_yaml(autoCheckpoint=True)
        assert err == ""
        mock_cfg_mod.save_config.assert_called_once()

    @patch("hermes.tools._persist_plugin_yaml.__module__", "hermes.tools")
    def test_plugins_not_dict(self):
        mock_cfg_mod = MagicMock()
        mock_cfg_mod.is_managed.return_value = False
        mock_cfg_mod.load_config.return_value = {"plugins": "bad"}
        with patch.dict("sys.modules", {"hermes_cli": MagicMock(), "hermes_cli.config": mock_cfg_mod}):
            err = _persist_plugin_yaml(workspace="/ws")
        assert "not a mapping" in err

    @patch("hermes.tools._persist_plugin_yaml.__module__", "hermes.tools")
    def test_ws_ckpt_not_dict(self):
        mock_cfg_mod = MagicMock()
        mock_cfg_mod.is_managed.return_value = False
        mock_cfg_mod.load_config.return_value = {"plugins": {"ws-ckpt": "bad"}}
        with patch.dict("sys.modules", {"hermes_cli": MagicMock(), "hermes_cli.config": mock_cfg_mod}):
            err = _persist_plugin_yaml(workspace="/ws")
        assert "not a mapping" in err

    @patch("hermes.tools._persist_plugin_yaml.__module__", "hermes.tools")
    def test_save_failure(self):
        mock_cfg_mod = MagicMock()
        mock_cfg_mod.is_managed.return_value = False
        mock_cfg_mod.load_config.return_value = {"plugins": {"ws-ckpt": {}}}
        mock_cfg_mod.save_config.side_effect = IOError("disk full")
        with patch.dict("sys.modules", {"hermes_cli": MagicMock(), "hermes_cli.config": mock_cfg_mod}):
            err = _persist_plugin_yaml(autoCheckpoint=True)
        assert "disk full" in err


# ---------------------------------------------------------------------------
# Handler failure paths
# ---------------------------------------------------------------------------


class TestHandlerFailurePaths:
    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(False, "error msg"))
    @patch("hermes.tools._reject_if_cwd_inside_workspace", return_value=None)
    @patch("hermes.tools._resolve_workspace", return_value=("/ws", None))
    def test_checkpoint_cmd_failure(self, _ws, _cwd, _cmd):
        result = json.loads(handle_ws_ckpt_checkpoint({"id": "snap1"}))
        assert result["success"] is False

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(False, "error"))
    @patch("hermes.tools._reject_if_cwd_inside_workspace", return_value=None)
    @patch("hermes.tools._resolve_workspace", return_value=("/ws", None))
    def test_rollback_cmd_failure(self, _ws, _cwd, _cmd):
        result = json.loads(handle_ws_ckpt_rollback({"target": "snap1"}))
        assert result["success"] is False

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(False, "error"))
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    def test_list_cmd_failure(self, _ws, _cmd):
        result = json.loads(handle_ws_ckpt_list({}))
        assert result["success"] is False

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(False, "error"))
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    def test_diff_cmd_failure(self, _ws, _cmd):
        result = json.loads(handle_ws_ckpt_diff({"from": "a", "to": "b"}))
        assert result["success"] is False

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(False, "error"))
    @patch("hermes.tools._resolve_workspace", return_value=("/ws", None))
    def test_delete_cmd_failure(self, _ws, _cmd):
        result = json.loads(handle_ws_ckpt_delete({"snapshot": "snap1"}))
        assert result["success"] is False

    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(False, "error"))
    @patch("hermes.tools._require_workspace", return_value=("/ws", None))
    def test_status_cmd_failure(self, _ws, _cmd):
        result = json.loads(handle_ws_ckpt_status({}))
        assert result["success"] is False

    # cwd rejection paths
    @patch("hermes.tools._reject_if_cwd_inside_workspace", return_value='{"success":false,"error":"cwd inside"}')
    @patch("hermes.tools._resolve_workspace", return_value=("/ws", None))
    def test_checkpoint_cwd_rejection(self, _ws, _cwd):
        result = json.loads(handle_ws_ckpt_checkpoint({"id": "snap1"}))
        assert result["success"] is False

    @patch("hermes.tools._reject_if_cwd_inside_workspace", return_value='{"success":false,"error":"cwd inside"}')
    @patch("hermes.tools._resolve_workspace", return_value=("/ws", None))
    def test_rollback_cwd_rejection(self, _ws, _cwd):
        result = json.loads(handle_ws_ckpt_rollback({"target": "snap1"}))
        assert result["success"] is False

    # explicit workspace in checkpoint
    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, "created"))
    @patch("hermes.tools._reject_if_cwd_inside_workspace", return_value=None)
    def test_checkpoint_explicit_workspace(self, _cwd, _cmd):
        result = json.loads(handle_ws_ckpt_checkpoint({"id": "s1", "workspace": "/explicit"}))
        assert result["success"] is True

    # explicit workspace in delete
    @patch("hermes.tools._run_ws_ckpt_cmd", return_value=(True, "deleted"))
    def test_delete_explicit_workspace(self, _cmd):
        result = json.loads(handle_ws_ckpt_delete({"snapshot": "s1", "workspace": "/explicit"}))
        assert result["success"] is True


# ---------------------------------------------------------------------------
# TOOLS tuple
# ---------------------------------------------------------------------------


class TestToolsTuple:
    def test_length(self):
        assert len(TOOLS) == 7

    def test_all_have_four_fields(self):
        for t in TOOLS:
            assert len(t) == 4
            name, schema, handler, emoji = t
            assert isinstance(name, str)
            assert isinstance(schema, dict)
            assert callable(handler)
            assert isinstance(emoji, str)
