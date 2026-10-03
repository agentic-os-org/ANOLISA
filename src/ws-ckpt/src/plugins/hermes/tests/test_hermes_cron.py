"""Tests for hermes cron module."""

from unittest.mock import MagicMock, patch, call

from hermes.cron import (
    validate_cron_expr,
    parse_schedules_update,
    _build_cron_line,
    _extract_workspace,
    _match_workspace,
    _read_crontab,
    _write_crontab,
    CrontabManager,
)


class TestValidateCronExpr:
    def test_valid(self):
        assert validate_cron_expr("0 * * * *") is True

    def test_with_whitespace(self):
        assert validate_cron_expr("  0 * * * *  ") is True

    def test_too_few_fields(self):
        assert validate_cron_expr("0 * * *") is False

    def test_too_many_fields(self):
        assert validate_cron_expr("0 * * * * *") is False

    def test_empty(self):
        assert validate_cron_expr("") is False

    def test_complex(self):
        assert validate_cron_expr("*/5 0-12 1,15 * 1-5") is True


class TestParseSchedulesUpdate:
    def test_add_valid(self):
        result, err = parse_schedules_update('add "0 * * * *"', [])
        assert err is None
        assert result == ["0 * * * *"]

    def test_add_empty(self):
        _, err = parse_schedules_update("add", [])
        assert err is not None

    def test_add_invalid(self):
        _, err = parse_schedules_update("add bad", [])
        assert err is not None
        assert "Invalid cron" in err

    def test_add_duplicate(self):
        result, _ = parse_schedules_update('add "0 * * * *"', ["0 * * * *"])
        assert result == ["0 * * * *"]

    def test_remove_existing(self):
        result, _ = parse_schedules_update("remove 0 * * * *", ["0 * * * *", "5 4 * * *"])
        assert result == ["5 4 * * *"]

    def test_remove_missing(self):
        _, err = parse_schedules_update("remove 0 * * * *", [])
        assert err is not None
        assert "not found" in err

    def test_remove_empty(self):
        _, err = parse_schedules_update("remove", [])
        assert err is not None

    def test_set_valid(self):
        result, _ = parse_schedules_update('set ["0 * * * *", "5 4 * * *"]', [])
        assert result == ["0 * * * *", "5 4 * * *"]

    def test_set_non_array(self):
        _, err = parse_schedules_update('set "0 * * * *"', [])
        assert err is not None
        assert "JSON array" in err

    def test_set_invalid_cron(self):
        _, err = parse_schedules_update('set ["bad"]', [])
        assert err is not None
        assert "Invalid cron" in err

    def test_set_invalid_json(self):
        _, err = parse_schedules_update("set not-json", [])
        assert err is not None

    def test_unknown_action(self):
        _, err = parse_schedules_update("delete 0 * * * *", [])
        assert err is not None
        assert "Unknown" in err

    def test_single_quote_stripping(self):
        result, _ = parse_schedules_update("add '0 * * * *'", [])
        assert result == ["0 * * * *"]


class TestBuildCronLine:
    def test_basic(self):
        line = _build_cron_line("/my/ws", "0 * * * *")
        assert line.startswith("0 * * * *")
        assert "ws-ckpt checkpoint" in line
        assert "-w '/my/ws'" in line

    def test_workspace_with_quote(self):
        line = _build_cron_line("/my/ws's", "0 * * * *")
        assert "'\\''" in line


class TestExtractWorkspace:
    def test_quoted(self):
        line = "0 * * * * ws-ckpt checkpoint -w '/my/ws' -i x"
        assert _extract_workspace(line) == "/my/ws"

    def test_unquoted(self):
        line = "0 * * * * ws-ckpt checkpoint -w /my/ws -i x"
        assert _extract_workspace(line) == "/my/ws"

    def test_non_ws_ckpt(self):
        assert _extract_workspace("0 * * * * echo hello") is None


class TestMatchWorkspace:
    def test_match(self):
        line = "0 * * * * ws-ckpt checkpoint -w '/ws' -i x"
        assert _match_workspace(line, "/ws") is True

    def test_no_match(self):
        line = "0 * * * * ws-ckpt checkpoint -w '/other' -i x"
        assert _match_workspace(line, "/ws") is False


class TestReadCrontab:
    @patch("hermes.cron.subprocess.run")
    def test_success(self, mock_run):
        mock_run.return_value = MagicMock(returncode=0, stdout="line1\nline2\n", stderr="")
        result = _read_crontab()
        assert result == ["line1", "line2"]

    @patch("hermes.cron.subprocess.run")
    def test_no_crontab(self, mock_run):
        mock_run.return_value = MagicMock(returncode=1, stderr="no crontab for user")
        assert _read_crontab() == []

    @patch("hermes.cron.subprocess.run")
    def test_other_error(self, mock_run):
        mock_run.return_value = MagicMock(returncode=1, stderr="permission denied")
        assert _read_crontab() is None

    @patch("hermes.cron.subprocess.run", side_effect=OSError("boom"))
    def test_exception(self, _):
        assert _read_crontab() is None


class TestWriteCrontab:
    @patch("hermes.cron.subprocess.run")
    def test_success(self, mock_run):
        mock_run.return_value = MagicMock(returncode=0)
        assert _write_crontab(["line1", "line2"]) is True
        args = mock_run.call_args
        assert args[1]["input"].endswith("\n")

    @patch("hermes.cron.subprocess.run")
    def test_failure(self, mock_run):
        mock_run.return_value = MagicMock(returncode=1)
        assert _write_crontab(["line1"]) is False

    @patch("hermes.cron.subprocess.run", side_effect=OSError)
    def test_exception(self, _):
        assert _write_crontab(["line1"]) is False


class TestCrontabManagerSync:
    @patch("hermes.cron.os.close")
    @patch("hermes.cron.os.open", return_value=99)
    @patch("hermes.cron.fcntl.flock")
    @patch("hermes.cron._write_crontab", return_value=True)
    @patch("hermes.cron._read_crontab")
    def test_replaces_old_entries(self, mock_read, mock_write, _flock, _open, _close):
        old_line = _build_cron_line("/ws", "0 * * * *")
        mock_read.return_value = ["# comment", old_line]
        result = CrontabManager.sync("/ws", ["5 4 * * *"])
        assert result is True
        written = mock_write.call_args[0][0]
        assert any("5 4 * * *" in l for l in written)
        assert not any("0 * * * *" in l and "ws-ckpt" in l for l in written)
        assert "# comment" in written

    @patch("hermes.cron.os.close")
    @patch("hermes.cron.os.open", return_value=99)
    @patch("hermes.cron.fcntl.flock")
    @patch("hermes.cron._read_crontab", return_value=None)
    def test_read_failure(self, _read, _flock, _open, _close):
        assert CrontabManager.sync("/ws", ["0 * * * *"]) is False


class TestCrontabManagerRemove:
    @patch("hermes.cron.os.close")
    @patch("hermes.cron.os.open", return_value=99)
    @patch("hermes.cron.fcntl.flock")
    @patch("hermes.cron._write_crontab", return_value=True)
    @patch("hermes.cron._read_crontab")
    def test_removes_entries(self, mock_read, mock_write, _flock, _open, _close):
        old_line = _build_cron_line("/ws", "0 * * * *")
        mock_read.return_value = [old_line, "other line"]
        CrontabManager.remove("/ws")
        written = mock_write.call_args[0][0]
        assert written == ["other line"]


class TestSyncWithRetry:
    @patch.object(CrontabManager, "sync", return_value=True)
    def test_success_first_try(self, mock_sync):
        assert CrontabManager.sync_with_retry("/ws", ["0 * * * *"]) is True
        assert mock_sync.call_count == 1

    @patch.object(CrontabManager, "sync", return_value=False)
    def test_all_retries_fail(self, mock_sync):
        assert CrontabManager.sync_with_retry("/ws", ["0 * * * *"], retries=2) is False
        assert mock_sync.call_count == 2


class TestRemoveWithRetry:
    @patch.object(CrontabManager, "sync_with_retry", return_value=True)
    def test_delegates(self, mock_sync):
        assert CrontabManager.remove_with_retry("/ws") is True
        mock_sync.assert_called_once_with("/ws", [], 3)


class TestMigrate:
    @patch.object(CrontabManager, "sync_with_retry", return_value=True)
    @patch.object(CrontabManager, "remove_with_retry", return_value=True)
    def test_same_workspace(self, mock_rm, mock_sync):
        warnings = CrontabManager.migrate("/ws", "/ws", ["0 * * * *"])
        mock_rm.assert_not_called()
        assert warnings == []

    @patch.object(CrontabManager, "sync_with_retry", return_value=True)
    @patch.object(CrontabManager, "remove_with_retry", return_value=False)
    def test_remove_failure(self, _rm, _sync):
        warnings = CrontabManager.migrate("/old", "/new", ["0 * * * *"])
        assert len(warnings) == 1
        assert "Failed to remove" in warnings[0]

    @patch.object(CrontabManager, "sync_with_retry", return_value=False)
    @patch.object(CrontabManager, "remove_with_retry", return_value=True)
    def test_install_failure(self, _rm, _sync):
        warnings = CrontabManager.migrate("/old", "/new", ["0 * * * *"])
        assert len(warnings) == 1
        assert "Failed to install" in warnings[0]

    @patch.object(CrontabManager, "sync_with_retry", return_value=True)
    @patch.object(CrontabManager, "remove_with_retry", return_value=True)
    def test_no_schedules(self, _rm, _sync):
        warnings = CrontabManager.migrate("/old", "/new", [])
        assert warnings == []
        _sync.assert_not_called()


class TestListInstalled:
    @patch("hermes.cron.os.close")
    @patch("hermes.cron.os.open", return_value=99)
    @patch("hermes.cron.fcntl.flock")
    @patch("hermes.cron._write_crontab", return_value=True)
    @patch("hermes.cron._read_crontab")
    def _sync_then_list(self, ws, schedules, mock_read, mock_write, *_):
        lines = [_build_cron_line(ws, s) for s in schedules]
        lines.append("# unrelated")
        mock_read.return_value = lines
        return CrontabManager.list_installed(ws)

    def test_returns_schedules(self):
        result = self._sync_then_list("/ws", ["0 * * * *", "5 4 * * *"])
        assert len(result) == 2
        assert "0 * * * *" in result
        assert "5 4 * * *" in result

    def test_empty_when_no_match(self):
        result = self._sync_then_list("/ws", [])
        assert result == []

    @patch("hermes.cron._read_crontab", return_value=None)
    def test_read_failure(self, _):
        assert CrontabManager.list_installed("/ws") == []
