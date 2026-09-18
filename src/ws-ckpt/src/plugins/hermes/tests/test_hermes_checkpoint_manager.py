"""Tests for hermes plugin checkpoint_manager module."""

from unittest.mock import MagicMock, patch

from hermes.checkpoint_manager import (
    CheckpointManager,
    CheckpointResult,
    CommandOutput,
    map_error_to_message,
)
from hermes.config import HermesPluginConfig


# ---------------------------------------------------------------------------
# map_error_to_message
# ---------------------------------------------------------------------------


class TestMapErrorToMessage:
    def test_binary_not_found(self):
        assert "CLI not found" in map_error_to_message("binary not found")

    def test_not_found_on_path(self):
        assert "CLI not found" in map_error_to_message("ws-ckpt not found on PATH")

    def test_timeout(self):
        assert "timed out" in map_error_to_message("Command timeout reached")

    def test_already_exists(self):
        assert "already exists" in map_error_to_message("snapshot already exists").lower()

    def test_active_write(self):
        msg = map_error_to_message("active write operations detected")
        assert "retry" in msg.lower()

    def test_write_operations(self):
        msg = map_error_to_message("workspace has write operations pending")
        assert "retry" in msg.lower()

    def test_insufficient(self):
        assert "disk space" in map_error_to_message("Insufficient disk space").lower()

    def test_cwd_scan_failed(self):
        msg = map_error_to_message("cwd scan failed: io error")
        assert "retryable" in msg.lower()

    def test_have_cwd_inside_workspace(self):
        msg = map_error_to_message("processes have cwd inside workspace")
        assert "NOT retryable" in msg

    def test_daemon_not_running(self):
        assert "not responding" in map_error_to_message("daemon is not running")

    def test_daemon_starting_up(self):
        assert "not responding" in map_error_to_message("daemon is starting up")

    def test_snapshot_not_found(self):
        assert "not found" in map_error_to_message("Snapshot not found").lower()

    def test_workspace_not_found(self):
        assert "not found" in map_error_to_message("Workspace not found").lower()

    def test_generic_error(self):
        msg = map_error_to_message("some random error")
        assert "some random error" in msg

    def test_with_context(self):
        msg = map_error_to_message("timeout occurred", {"id": "snap1"})
        assert "snap1" in msg

    def test_binary_not_found_priority_over_snapshot(self):
        msg = map_error_to_message("binary not found on path, snapshot not found")
        assert "CLI not found" in msg


# ---------------------------------------------------------------------------
# Dataclasses
# ---------------------------------------------------------------------------


class TestDataclasses:
    def test_command_output(self):
        out = CommandOutput(exit_code=0, stdout="ok", stderr="")
        assert out.exit_code == 0
        assert out.stdout == "ok"

    def test_checkpoint_result_defaults(self):
        r = CheckpointResult(success=True, message="done")
        assert r.snapshot == ""
        assert r.skipped is False
        assert r.reason is None


# ---------------------------------------------------------------------------
# CheckpointManager
# ---------------------------------------------------------------------------


class TestCheckpointManager:
    def _make(self, workspace="/ws", auto=False):
        cfg = HermesPluginConfig(workspace=workspace, auto_checkpoint=auto)
        return CheckpointManager(cfg)

    def test_config_property(self):
        mgr = self._make("/my/ws", True)
        assert mgr.config.workspace == "/my/ws"
        assert mgr.config.auto_checkpoint is True

    def test_set_workspace(self):
        mgr = self._make()
        mgr.set_workspace("/new")
        assert mgr.config.workspace == "/new"

    def test_set_auto_checkpoint(self):
        mgr = self._make()
        mgr.set_auto_checkpoint(True)
        assert mgr.config.auto_checkpoint is True

    def test_advance_turn(self):
        mgr = self._make()
        assert mgr.advance_turn() == 1
        assert mgr.advance_turn() == 2
        assert mgr.advance_turn() == 3

    @patch("hermes.checkpoint_manager.subprocess.run")
    def test_run_success(self, mock_run):
        mock_run.return_value = MagicMock(returncode=0, stdout="ok", stderr="")
        mgr = self._make("/ws")
        out = mgr.init_workspace()
        assert out.exit_code == 0
        assert out.stdout == "ok"
        assert mock_run.call_args.kwargs["timeout"] == 240

    @patch("hermes.checkpoint_manager.subprocess.run")
    def test_run_timeout(self, mock_run):
        import subprocess
        mock_run.side_effect = subprocess.TimeoutExpired(cmd="ws-ckpt", timeout=240)
        mgr = self._make("/ws")
        out = mgr.init_workspace()
        assert out.exit_code == 1
        assert "timed out" in out.stderr.lower()

    @patch("hermes.checkpoint_manager.subprocess.run")
    def test_run_file_not_found(self, mock_run):
        mock_run.side_effect = FileNotFoundError()
        mgr = self._make("/ws")
        out = mgr.init_workspace()
        assert out.exit_code == 127
        assert "not found" in out.stderr.lower()

    @patch("hermes.checkpoint_manager.subprocess.run")
    def test_run_generic_exception(self, mock_run):
        mock_run.side_effect = RuntimeError("boom")
        mgr = self._make("/ws")
        out = mgr.init_workspace()
        assert out.exit_code == 1
        assert "boom" in out.stderr

    @patch("hermes.checkpoint_manager.subprocess.run")
    def test_init_workspace_args(self, mock_run):
        mock_run.return_value = MagicMock(returncode=0, stdout="", stderr="")
        mgr = self._make("/my/workspace")
        mgr.init_workspace()
        args = mock_run.call_args[0][0]
        assert args == ["ws-ckpt", "init", "--workspace", "/my/workspace"]

    @patch("hermes.checkpoint_manager.subprocess.run")
    def test_create_checkpoint_success(self, mock_run):
        mock_run.return_value = MagicMock(returncode=0, stdout="", stderr="")
        mgr = self._make("/ws")
        result = mgr.create_checkpoint(snapshot_id="abc123", message="test")
        assert result.success is True
        assert result.snapshot == "abc123"

    @patch("hermes.checkpoint_manager.subprocess.run")
    def test_create_checkpoint_skipped_from_stderr(self, mock_run):
        mock_run.return_value = MagicMock(
            returncode=0,
            stdout="",
            stderr="\x1b[33m⚠ Empty workspace, no snapshot created.\x1b[0m\n",
        )
        mgr = self._make("/ws")
        result = mgr.create_checkpoint(snapshot_id="empty")
        assert result.success is True
        assert result.skipped is True
        assert result.snapshot == ""
        assert result.reason == "Empty workspace, no snapshot created."

    @patch("hermes.checkpoint_manager.subprocess.run")
    def test_create_checkpoint_with_metadata(self, mock_run):
        mock_run.return_value = MagicMock(returncode=0, stdout="", stderr="")
        mgr = self._make("/ws")
        mgr.create_checkpoint(snapshot_id="s1", message="m", metadata={"turn": 1})
        args = mock_run.call_args[0][0]
        assert "--metadata" in args

    @patch("hermes.checkpoint_manager.subprocess.run")
    def test_create_checkpoint_failure(self, mock_run):
        mock_run.return_value = MagicMock(returncode=1, stdout="", stderr="snapshot already exists")
        mgr = self._make("/ws")
        result = mgr.create_checkpoint(snapshot_id="dup")
        assert result.success is False
        assert "already exists" in result.message.lower()

    @patch("hermes.checkpoint_manager.subprocess.run")
    def test_create_checkpoint_no_message(self, mock_run):
        mock_run.return_value = MagicMock(returncode=0, stdout="", stderr="")
        mgr = self._make("/ws")
        mgr.create_checkpoint(snapshot_id="s1")
        args = mock_run.call_args[0][0]
        assert "--message" not in args

    @patch("hermes.checkpoint_manager.subprocess.run")
    def test_create_checkpoint_truncates_message(self, mock_run):
        mock_run.return_value = MagicMock(returncode=0, stdout="", stderr="")
        mgr = self._make("/ws")
        long_msg = "x" * 200
        mgr.create_checkpoint(snapshot_id="s1", message=long_msg)
        args = mock_run.call_args[0][0]
        msg_idx = args.index("--message") + 1
        assert len(args[msg_idx]) == 80
