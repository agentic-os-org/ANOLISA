"""The SDK hands control only to the enabled plugin in the selected Hermes profile."""

from pathlib import Path
import sys
import tempfile
from threading import RLock
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from aw_sdk.hermes import listen
from aw_sdk import AwError


class HermesListener(unittest.TestCase):
    def test_plugin_selection_and_activation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = root / "plugins" / "aw-native-hooks"
            installed.mkdir(parents=True)
            module = Mock()
            plugin = SimpleNamespace(enabled=True, error=None, manifest=SimpleNamespace(path=installed),
                                     module=module)
            manager = SimpleNamespace(_plugins={"aw": plugin}, _discovery_lock=RLock())
            modules = {"hermes_cli.plugins": SimpleNamespace(get_plugin_manager=lambda: manager,
                           discover_plugins=lambda: None),
                       "hermes_constants": SimpleNamespace(get_hermes_home=lambda: root)}
            runner = object()
            with patch.dict(sys.modules, modules):
                options = dict(config_file="/config", descriptor_file="/descriptor", target="worker")
                self.assertIs(listen(runner, **options), module.start_sdk.return_value)
                module.start_sdk.assert_called_once_with(runner, "/config", "/descriptor", "worker", 5000)
                with self.assertRaisesRegex(AwError, "invalid_attach_timeout"):
                    listen(runner, **options, attach_timeout_ms=0)
                for enabled, error, path in [(False, None, installed), (True, "failed", installed),
                                              (True, None, root / "other")]:
                    plugin.enabled, plugin.error, plugin.manifest.path = enabled, error, path
                    with self.assertRaisesRegex(AwError, "hermes_plugin_unavailable"):
                        listen(runner, **options)
