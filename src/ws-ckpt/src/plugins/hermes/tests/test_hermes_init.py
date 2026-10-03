"""Tests for hermes plugin __init__ module (hooks + register)."""

import threading
from unittest.mock import MagicMock, patch, PropertyMock

from hermes.config import HermesPluginConfig
from hermes.checkpoint_manager import CheckpointManager, CheckpointResult, CommandOutput


# ---------------------------------------------------------------------------
# _cwd_inside_workspace
# ---------------------------------------------------------------------------


class TestCwdInsideWorkspace:
    @patch("hermes.checkpoint_manager.os.getcwd", return_value="/ws/subdir")
    def test_inside(self, _):
        from hermes.checkpoint_manager import cwd_inside_workspace
        inside, cwd = cwd_inside_workspace("/ws")
        assert inside is True
        assert cwd == "/ws/subdir"

    @patch("hermes.checkpoint_manager.os.getcwd", return_value="/ws")
    def test_exact_match(self, _):
        from hermes.checkpoint_manager import cwd_inside_workspace
        inside, _ = cwd_inside_workspace("/ws")
        assert inside is True

    @patch("hermes.checkpoint_manager.os.getcwd", return_value="/other")
    def test_outside(self, _):
        from hermes.checkpoint_manager import cwd_inside_workspace
        inside, _ = cwd_inside_workspace("/ws")
        assert inside is False

    @patch("hermes.checkpoint_manager.os.getcwd", side_effect=FileNotFoundError)
    def test_getcwd_fails(self, _):
        from hermes.checkpoint_manager import cwd_inside_workspace
        inside, cwd = cwd_inside_workspace("/ws")
        assert inside is False
        assert cwd == ""


class TestCwdInsideWorkspaceReason:
    def test_format(self):
        from hermes.checkpoint_manager import cwd_inside_workspace_reason
        msg = cwd_inside_workspace_reason("/ws/sub", "/ws")
        assert "cwd=" in msg
        assert "workspace=" in msg
        assert "inode" in msg


# ---------------------------------------------------------------------------
# _on_pre_llm_call
# ---------------------------------------------------------------------------


class TestOnPreLlmCall:
    def test_captures_message(self):
        import hermes as h
        h._on_pre_llm_call(user_message="hello world")
        assert h._last_user_message == "hello world"


# ---------------------------------------------------------------------------
# _on_session_end
# ---------------------------------------------------------------------------


class TestOnSessionEnd:
    @patch("hermes.get_manager")
    def test_skips_when_auto_disabled(self, mock_get_mgr):
        cfg = HermesPluginConfig(workspace="/ws", auto_checkpoint=False)
        mgr = MagicMock(spec=CheckpointManager)
        type(mgr).config = PropertyMock(return_value=cfg)
        mgr.skip_next_auto_checkpoint = False
        mock_get_mgr.return_value = mgr
        import hermes as h
        h._on_session_end()
        mgr.create_checkpoint.assert_not_called()

    @patch("hermes.get_manager")
    def test_truncates_long_message(self, mock_get_mgr):
        cfg = HermesPluginConfig(workspace="/ws", auto_checkpoint=True)
        mgr = MagicMock(spec=CheckpointManager)
        type(mgr).config = PropertyMock(return_value=cfg)
        mgr.skip_next_auto_checkpoint = False
        mgr.advance_turn.return_value = 1
        mgr.create_checkpoint.return_value = CheckpointResult(
            success=True, message="ok", snapshot="s1"
        )
        mock_get_mgr.return_value = mgr

        import hermes as h
        with h._msg_lock:
            pass  # ensure lock is free
        h._last_user_message = "x" * 200
        h._on_session_end()

        call_kwargs = mgr.create_checkpoint.call_args[1]
        assert len(call_kwargs["message"]) == 83  # 80 + "..."

    @patch("hermes.get_manager")
    def test_empty_message_fallback(self, mock_get_mgr):
        cfg = HermesPluginConfig(workspace="/ws", auto_checkpoint=True)
        mgr = MagicMock(spec=CheckpointManager)
        type(mgr).config = PropertyMock(return_value=cfg)
        mgr.skip_next_auto_checkpoint = False
        mgr.advance_turn.return_value = 1
        mgr.create_checkpoint.return_value = CheckpointResult(
            success=True, message="ok", snapshot="s1"
        )
        mock_get_mgr.return_value = mgr

        import hermes as h
        h._last_user_message = ""
        h._on_session_end()

        call_kwargs = mgr.create_checkpoint.call_args[1]
        assert call_kwargs["message"] == "agent turn"


# ---------------------------------------------------------------------------
# _on_session_start
# ---------------------------------------------------------------------------


class TestOnSessionStart:
    @patch("hermes.get_manager")
    def test_skips_when_auto_disabled(self, mock_get_mgr):
        cfg = HermesPluginConfig(workspace="/ws", auto_checkpoint=False)
        mgr = MagicMock(spec=CheckpointManager)
        type(mgr).config = PropertyMock(return_value=cfg)
        mock_get_mgr.return_value = mgr
        import hermes as h
        h._on_session_start()
        mgr.init_workspace.assert_not_called()

    @patch("hermes.get_manager")
    def test_disables_when_no_workspace(self, mock_get_mgr):
        cfg = HermesPluginConfig(workspace="", auto_checkpoint=True)
        mgr = MagicMock(spec=CheckpointManager)
        type(mgr).config = PropertyMock(return_value=cfg)
        mock_get_mgr.return_value = mgr
        import hermes as h
        h._on_session_start()
        mgr.set_auto_checkpoint.assert_called_once_with(False)

    @patch("hermes.checkpoint_manager.os.getcwd", return_value="/ws/sub")
    @patch("hermes.get_manager")
    def test_disables_when_cwd_inside_workspace(self, mock_get_mgr, _):
        cfg = HermesPluginConfig(workspace="/ws", auto_checkpoint=True)
        mgr = MagicMock(spec=CheckpointManager)
        type(mgr).config = PropertyMock(return_value=cfg)
        mock_get_mgr.return_value = mgr
        import hermes as h
        h._on_session_start()
        mgr.set_auto_checkpoint.assert_called_once_with(False)

    @patch("hermes.checkpoint_manager.os.getcwd", return_value="/other")
    @patch("hermes.get_manager")
    def test_init_failure(self, mock_get_mgr, _):
        cfg = HermesPluginConfig(workspace="/ws", auto_checkpoint=True)
        mgr = MagicMock(spec=CheckpointManager)
        type(mgr).config = PropertyMock(return_value=cfg)
        mgr.init_workspace.return_value = CommandOutput(exit_code=1, stdout="", stderr="fail")
        mock_get_mgr.return_value = mgr
        import hermes as h
        h._on_session_start()
        mgr.create_checkpoint.assert_not_called()

    @patch("hermes.checkpoint_manager.os.getcwd", return_value="/other")
    @patch("hermes.get_manager")
    def test_success(self, mock_get_mgr, _):
        cfg = HermesPluginConfig(workspace="/ws", auto_checkpoint=True)
        mgr = MagicMock(spec=CheckpointManager)
        type(mgr).config = PropertyMock(return_value=cfg)
        mgr.init_workspace.return_value = CommandOutput(exit_code=0, stdout="ok", stderr="")
        mgr.create_checkpoint.return_value = CheckpointResult(
            success=True, message="ok", snapshot="s1"
        )
        mock_get_mgr.return_value = mgr
        import hermes as h
        h._on_session_start()
        mgr.create_checkpoint.assert_called_once()


# ---------------------------------------------------------------------------
# register
# ---------------------------------------------------------------------------


class TestRegister:
    def test_registers_hooks_and_tools(self):
        import hermes as h
        ctx = MagicMock()
        h.register(ctx)
        assert ctx.register_hook.call_count == 3
        assert ctx.register_tool.call_count == 7
