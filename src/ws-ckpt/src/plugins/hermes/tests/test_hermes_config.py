"""Tests for hermes plugin config module."""

import os
from unittest.mock import patch

import pytest

from hermes.config import HermesPluginConfig, MSG_TRUNCATE_LEN, load_config


class TestMsgTruncateLen:
    def test_value(self):
        assert MSG_TRUNCATE_LEN == 80


class TestHermesPluginConfig:
    def test_dataclass_fields(self):
        cfg = HermesPluginConfig(workspace="/tmp/ws", auto_checkpoint=True)
        assert cfg.workspace == "/tmp/ws"
        assert cfg.auto_checkpoint is True


class TestLoadConfig:
    @patch("hermes.config._read_yaml_config", return_value={})
    def test_defaults_no_env(self, _mock_yaml):
        with patch.dict(os.environ, {}, clear=True):
            cfg = load_config()
        assert cfg.workspace == ""
        assert cfg.auto_checkpoint is False

    @patch("hermes.config._read_yaml_config", return_value={})
    def test_env_workspace(self, _mock_yaml):
        with patch.dict(os.environ, {"WS_CKPT_WORKSPACE": "/env/ws"}, clear=True):
            cfg = load_config()
        assert cfg.workspace == "/env/ws"

    @patch("hermes.config._read_yaml_config", return_value={"workspace": "/yaml/ws"})
    def test_yaml_workspace_fallback(self, _mock_yaml):
        with patch.dict(os.environ, {}, clear=True):
            cfg = load_config()
        assert cfg.workspace == "/yaml/ws"

    @patch("hermes.config._read_yaml_config", return_value={"workspace": "/yaml/ws"})
    def test_env_overrides_yaml_workspace(self, _mock_yaml):
        with patch.dict(os.environ, {"WS_CKPT_WORKSPACE": "/env/ws"}, clear=True):
            cfg = load_config()
        assert cfg.workspace == "/env/ws"

    @patch("hermes.config._read_yaml_config", return_value={})
    @pytest.mark.parametrize("val", ["true", "1", "yes", "on"])
    def test_env_auto_checkpoint_truthy(self, _mock_yaml, val):
        with patch.dict(os.environ, {"WS_CKPT_AUTO_CHECKPOINT": val}, clear=True):
            cfg = load_config()
        assert cfg.auto_checkpoint is True

    @patch("hermes.config._read_yaml_config", return_value={})
    @pytest.mark.parametrize("val", ["false", "0", "no", "off", "random"])
    def test_env_auto_checkpoint_falsy(self, _mock_yaml, val):
        with patch.dict(os.environ, {"WS_CKPT_AUTO_CHECKPOINT": val}, clear=True):
            cfg = load_config()
        assert cfg.auto_checkpoint is False

    @patch("hermes.config._read_yaml_config", return_value={"autoCheckpoint": True})
    def test_yaml_auto_checkpoint(self, _mock_yaml):
        with patch.dict(os.environ, {}, clear=True):
            cfg = load_config()
        assert cfg.auto_checkpoint is True

    @patch("hermes.config._read_yaml_config", return_value={"autoCheckpoint": True})
    def test_env_overrides_yaml_auto_checkpoint(self, _mock_yaml):
        with patch.dict(os.environ, {"WS_CKPT_AUTO_CHECKPOINT": "false"}, clear=True):
            cfg = load_config()
        assert cfg.auto_checkpoint is False

    @patch("hermes.config._read_yaml_config", return_value={"workspace": "  "})
    def test_whitespace_workspace_treated_as_empty(self, _mock_yaml):
        with patch.dict(os.environ, {}, clear=True):
            cfg = load_config()
        assert cfg.workspace == ""

    @patch("hermes.config._read_yaml_config", return_value={"workspace": None})
    def test_none_workspace(self, _mock_yaml):
        with patch.dict(os.environ, {}, clear=True):
            cfg = load_config()
        assert cfg.workspace == ""

    @patch("hermes.config._read_yaml_config", return_value={"cronSchedules": ["0 * * * *", "5 4 * * *"]})
    def test_cron_schedules_valid(self, _mock_yaml):
        with patch.dict(os.environ, {}, clear=True):
            cfg = load_config()
        assert cfg.cron_schedules == ["0 * * * *", "5 4 * * *"]

    @patch("builtins.print")
    @patch("hermes.config._read_yaml_config", return_value={"cronSchedules": ["0 * * * *", "bad"]})
    def test_cron_schedules_invalid_filtered(self, _mock_yaml, mock_print):
        with patch.dict(os.environ, {}, clear=True):
            cfg = load_config()
        assert cfg.cron_schedules == ["0 * * * *"]
        mock_print.assert_called_once()
        assert "bad" in mock_print.call_args[0][0]

    @patch("hermes.config._read_yaml_config", return_value={"cronSchedules": "not a list"})
    def test_cron_schedules_not_list_ignored(self, _mock_yaml):
        with patch.dict(os.environ, {}, clear=True):
            cfg = load_config()
        assert cfg.cron_schedules == []

    @patch("hermes.config._read_yaml_config", return_value={"cronSchedules": []})
    def test_cron_schedules_empty(self, _mock_yaml):
        with patch.dict(os.environ, {}, clear=True):
            cfg = load_config()
        assert cfg.cron_schedules == []

    @patch("hermes.config._read_yaml_config", return_value={})
    def test_cron_schedules_default(self, _mock_yaml):
        with patch.dict(os.environ, {}, clear=True):
            cfg = load_config()
        assert cfg.cron_schedules == []
