"""Tests for hermes plugin config module."""

import os
import sys
import types
from unittest.mock import patch

import pytest

from hermes.config import (
    HermesPluginConfig,
    MSG_TRUNCATE_LEN,
    _read_yaml_config,
    load_config,
)


def _hermes_cli_with(section):
    """A stand-in `hermes_cli` whose `plugins.ws-ckpt` section is `section`.

    `_read_yaml_config` imports `hermes_cli.config` at call time, so patching
    `sys.modules` is enough and the real module stays importable elsewhere.
    """
    package = types.ModuleType("hermes_cli")
    package.__path__ = []  # `from hermes_cli.config import ...` needs a package
    config_module = types.ModuleType("hermes_cli.config")
    config_module.load_config = lambda: {"plugins": {"ws-ckpt": section}}
    config_module.cfg_get = lambda config, *keys, default=None: (
        config.get(keys[0]).get(keys[1], default)
        if isinstance(config.get(keys[0]), dict)
        else default
    )
    package.config = config_module
    return {"hermes_cli": package, "hermes_cli.config": config_module}


class TestNonMappingYamlSection:
    """`_read_yaml_config` must yield a mapping whatever the section holds.

    The section comes from a file a user edits by hand, so any YAML value can
    appear there. The docstring contracts "dict, or {} on failure"; `or {}`
    covered only falsy values, so a scalar or list reached `load_config`, which
    calls `.get()` on it and raised AttributeError during plugin import --
    taking down every ws-ckpt tool and hook, not just the workspace field.
    """

    @pytest.mark.parametrize("section", ["just-a-scalar", ["a", "b"], 7, True])
    def test_truthy_non_mapping_section_reads_as_empty(self, section):
        # Every value here is truthy, so `or {}` passed it through unchanged.
        with patch.dict(sys.modules, _hermes_cli_with(section)), \
                patch.dict(os.environ, {}, clear=True):
            assert _read_yaml_config() == {}

    def test_load_config_survives_a_non_mapping_section(self):
        # Without the isinstance guard this raises
        # AttributeError: 'str' object has no attribute 'get'.
        with patch.dict(sys.modules, _hermes_cli_with("just-a-scalar")), \
                patch.dict(os.environ, {}, clear=True):
            cfg = load_config()
        assert cfg.workspace == ""
        assert cfg.auto_checkpoint is False

    def test_a_mapping_section_still_reaches_load_config(self):
        # Keeps the two cases above honest: if the stub stopped taking effect,
        # `_read_yaml_config` would fall into `except Exception` and both would
        # pass for the wrong reason.
        section = {"workspace": "/tmp/project", "autoCheckpoint": True}
        with patch.dict(sys.modules, _hermes_cli_with(section)), \
                patch.dict(os.environ, {}, clear=True):
            assert _read_yaml_config() == section
            cfg = load_config()
        assert cfg.workspace == "/tmp/project"
        assert cfg.auto_checkpoint is True


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
