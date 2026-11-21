"""Round-trip coverage for apostrophe workspace paths in crontab entries.

Issue #10469: the writer escapes every single quote as ``'\\''``
(``_build_cron_line``), but the extractor stopped at the first ``'`` and never
unescaped, so ``/tmp/alice's-project`` was truncated to ``/tmp/alice``.  The
consequences were duplicate crontab lines on every sync, empty
``list_installed`` results, and cross-workspace removals (syncing or removing
``/tmp/alice`` matched the escaped line of ``/tmp/alice's-project``).

These tests pin the writer -> crontab line -> extractor round trip in both
implementations of the contract, including the exact line bytes reported in
the issue.
"""

from unittest.mock import patch

import pytest

from hermes.cron import (
    CrontabManager,
    _build_cron_line,
    _extract_workspace,
    _match_workspace,
)

SCHEDULE = "0 3 * * *"
QUOTED_WS = "/tmp/alice's-project"

# The exact crontab line bytes the writer produces for QUOTED_WS, written out
# literally so the extractor is checked against the reported format rather
# than only against _build_cron_line output.
ISSUE_LINE = (
    "0 3 * * * /usr/bin/ws-ckpt checkpoint -w '/tmp/alice'\\''s-project'"
    ' -s "cron-$(date +\\%s)" -m "scheduled snapshot"'
    " --metadata '{\"auto\":true,\"type\":\"cron\"}' >/dev/null 2>&1"
)


@pytest.fixture(autouse=True)
def installed_ws_ckpt_binary():
    with patch("hermes.cron.shutil.which", return_value="/usr/bin/ws-ckpt"):
        yield


class TestQuotedWorkspaceRoundTrip:
    """writer -> crontab line -> extractor must restore the original path."""

    @pytest.mark.parametrize(
        "workspace",
        [
            "/tmp/alice's-project",
            "/tmp/it's \"ok\" dir",
            "/tmp/x'",  # trailing quote: the escape ends the token
            "'",  # quote-only path
            "''",  # two quotes in a row
            "/a\\'b",  # backslash immediately before a quote
            "/tmp/weird'path''here",
            "/opt/o'brien's lab (2)",
        ],
    )
    def test_build_then_extract_restores_path(self, workspace):
        line = _build_cron_line(workspace, SCHEDULE, "/usr/bin/ws-ckpt")
        assert _extract_workspace(line) == workspace

    def test_issue_reported_line_extracts_full_path(self):
        assert _extract_workspace(ISSUE_LINE) == QUOTED_WS

    def test_issue_line_is_the_writer_format(self):
        assert ISSUE_LINE == _build_cron_line(
            QUOTED_WS, SCHEDULE, "/usr/bin/ws-ckpt"
        )

    def test_plain_quoted_path_still_extracts(self):
        line = "0 * * * * ws-ckpt checkpoint -w '/my/ws' -i x"
        assert _extract_workspace(line) == "/my/ws"

    def test_unquoted_fallback_still_extracts(self):
        line = "0 * * * * ws-ckpt checkpoint -w /my/ws -i x"
        assert _extract_workspace(line) == "/my/ws"

    def test_non_ws_ckpt_line_has_no_workspace(self):
        assert _extract_workspace("0 * * * * echo hello") is None


class TestQuotedWorkspaceOwnership:
    """The escaped line belongs to its exact workspace only."""

    def test_exact_workspace_matches_its_line(self):
        line = _build_cron_line(QUOTED_WS, SCHEDULE, "/usr/bin/ws-ckpt")
        assert _match_workspace(line, QUOTED_WS) is True

    def test_prefix_workspace_does_not_clobber_escaped_line(self):
        line = _build_cron_line(QUOTED_WS, SCHEDULE, "/usr/bin/ws-ckpt")
        assert _match_workspace(line, "/tmp/alice") is False

    def test_escaped_line_does_not_match_prefix_workspace(self):
        line = _build_cron_line("/tmp/alice", SCHEDULE, "/usr/bin/ws-ckpt")
        assert _match_workspace(line, QUOTED_WS) is False


class TestCrontabManagerQuotedWorkspace:
    """Manager-level sync/remove/list/migrate with an apostrophe workspace."""

    @patch("hermes.cron.os.close")
    @patch("hermes.cron.os.open", return_value=99)
    @patch("hermes.cron.fcntl.flock")
    @patch("hermes.cron._write_crontab", return_value=True)
    @patch("hermes.cron._read_crontab")
    def test_sync_is_idempotent_for_quoted_workspace(
        self, mock_read, mock_write, _flock, _open, _close
    ):
        existing = [
            _build_cron_line(QUOTED_WS, "5 4 * * *", "/usr/bin/ws-ckpt"),
            _build_cron_line("/plain-ws", "0 * * * *", "/usr/bin/ws-ckpt"),
        ]
        mock_read.return_value = existing

        assert CrontabManager.sync(QUOTED_WS, [SCHEDULE]) is True

        written = mock_write.call_args[0][0]
        # The stale 5 4 * * * entry is replaced; the plain workspace survives.
        assert written == [
            _build_cron_line("/plain-ws", "0 * * * *", "/usr/bin/ws-ckpt"),
            _build_cron_line(QUOTED_WS, SCHEDULE, "/usr/bin/ws-ckpt"),
        ]

        # Resyncing against the just-written crontab must not append again.
        mock_read.return_value = list(written)
        assert CrontabManager.sync(QUOTED_WS, [SCHEDULE]) is True
        assert mock_write.call_args[0][0] == written

    @patch("hermes.cron.os.close")
    @patch("hermes.cron.os.open", return_value=99)
    @patch("hermes.cron.fcntl.flock")
    @patch("hermes.cron._write_crontab", return_value=True)
    @patch("hermes.cron._read_crontab")
    def test_sync_with_prefix_workspace_keeps_escaped_entries(
        self, mock_read, mock_write, _flock, _open, _close
    ):
        escaped_line = _build_cron_line(QUOTED_WS, SCHEDULE, "/usr/bin/ws-ckpt")
        mock_read.return_value = [escaped_line]

        assert CrontabManager.sync("/tmp/alice", [SCHEDULE]) is True

        written = mock_write.call_args[0][0]
        assert escaped_line in written
        assert len([l for l in written if "-w" in l]) == 2

    @patch("hermes.cron.os.close")
    @patch("hermes.cron.os.open", return_value=99)
    @patch("hermes.cron.fcntl.flock")
    @patch("hermes.cron._write_crontab", return_value=True)
    @patch("hermes.cron._read_crontab")
    def test_remove_clears_quoted_workspace_entries(
        self, mock_read, mock_write, _flock, _open, _close
    ):
        mock_read.return_value = [
            _build_cron_line(QUOTED_WS, "5 4 * * *", "/usr/bin/ws-ckpt"),
            _build_cron_line(QUOTED_WS, SCHEDULE, "/usr/bin/ws-ckpt"),
            _build_cron_line("/plain-ws", "0 * * * *", "/usr/bin/ws-ckpt"),
        ]

        assert CrontabManager.remove(QUOTED_WS) is True

        assert mock_write.call_args[0][0] == [
            _build_cron_line("/plain-ws", "0 * * * *", "/usr/bin/ws-ckpt"),
        ]

    @patch("hermes.cron._read_crontab")
    def test_list_installed_returns_quoted_workspace_schedules(self, mock_read):
        mock_read.return_value = [
            _build_cron_line(QUOTED_WS, "5 4 * * *", "/usr/bin/ws-ckpt"),
            _build_cron_line(QUOTED_WS, SCHEDULE, "/usr/bin/ws-ckpt"),
            _build_cron_line("/tmp/alice", "1 2 * * *", "/usr/bin/ws-ckpt"),
        ]
        assert CrontabManager.list_installed(QUOTED_WS) == [
            "5 4 * * *",
            SCHEDULE,
        ]

    @patch("hermes.cron._read_crontab")
    def test_list_installed_ignores_prefix_workspace(self, mock_read):
        mock_read.return_value = [
            _build_cron_line(QUOTED_WS, SCHEDULE, "/usr/bin/ws-ckpt"),
        ]
        assert CrontabManager.list_installed("/tmp/alice") == []

    @patch("hermes.cron.os.close")
    @patch("hermes.cron.os.open", return_value=99)
    @patch("hermes.cron.fcntl.flock")
    @patch("hermes.cron._write_crontab", return_value=True)
    @patch("hermes.cron._read_crontab")
    def test_migrate_moves_quoted_workspace_entries(
        self, mock_read, mock_write, _flock, _open, _close
    ):
        old_line = _build_cron_line(QUOTED_WS, SCHEDULE, "/usr/bin/ws-ckpt")
        # First read serves the removal phase, the second the installation
        # phase (after the old entries were written away).
        mock_read.side_effect = [[old_line], []]
        new_ws = "/tmp/bob's-projects"

        warnings = CrontabManager.migrate(QUOTED_WS, new_ws, [SCHEDULE])

        assert warnings == []
        removed, written = [call[0][0] for call in mock_write.call_args_list]
        assert removed == []
        assert written == [
            _build_cron_line(new_ws, SCHEDULE, "/usr/bin/ws-ckpt")
        ]
