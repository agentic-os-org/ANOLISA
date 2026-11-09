#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""`_read_yaml_config` must return a mapping whatever lives under plugins.ws-ckpt.

The section comes from a file a user edits by hand, so any YAML value can appear
there. The contract in the docstring is "dict, or {} on failure"; `or {}` covered
only falsy values, so a scalar or list reached `load_config`, which calls
`.get()` on it and raised AttributeError during plugin import -- taking down
every ws-ckpt tool and hook, not just the workspace field.
"""

import importlib.util
import sys
import types
import unittest
from pathlib import Path

HERMES_DIR = Path(__file__).resolve().parents[1] / "src" / "plugins" / "hermes"
CONFIG_PATH = HERMES_DIR / "config.py"
PACKAGE = "ws_ckpt_hermes_under_test"


def import_plugin_config(testcase, section):
    """Imports the plugin module with `plugins.ws-ckpt` stubbed to `section`.

    The module is loaded as part of a synthetic package so its `from .cron
    import ...` resolves against the real sibling, and the `hermes_cli` stub
    stays installed until the test finishes, because the import happens inside
    `_read_yaml_config` at call time rather than at module load.
    """
    hermes_cli = types.ModuleType("hermes_cli")
    hermes_cli.__path__ = []  # `from hermes_cli.config import ...` needs a package
    config_module = types.ModuleType("hermes_cli.config")
    config_module.load_config = lambda: {"plugins": {"ws-ckpt": section}}
    config_module.cfg_get = lambda config, *keys, default=None: (
        config.get(keys[0]).get(keys[1], default)
        if isinstance(config.get(keys[0]), dict)
        else default
    )
    hermes_cli.config = config_module

    package = types.ModuleType(PACKAGE)
    package.__path__ = [str(HERMES_DIR)]

    stubs = {
        "hermes_cli": hermes_cli,
        "hermes_cli.config": config_module,
        PACKAGE: package,
    }
    saved = {name: sys.modules.get(name) for name in stubs}

    def restore():
        for name, previous in saved.items():
            if previous is None:
                sys.modules.pop(name, None)
            else:
                sys.modules[name] = previous

    sys.modules.update(stubs)
    testcase.addCleanup(restore)

    spec = importlib.util.spec_from_file_location(f"{PACKAGE}.config", CONFIG_PATH)
    module = importlib.util.module_from_spec(spec)
    sys.modules[f"{PACKAGE}.config"] = module
    spec.loader.exec_module(module)
    return module


class NonMappingSectionTest(unittest.TestCase):
    def test_a_truthy_non_mapping_section_reads_as_empty(self):
        # The reported failure: every one of these is truthy, so `or {}` let it
        # through to `load_config` unchanged.
        for section in ("just-a-scalar", ["a", "b"], 7, True):
            with self.subTest(section=section):
                module = import_plugin_config(self, section)

                self.assertEqual(module._read_yaml_config(), {})

    def test_load_config_survives_a_non_mapping_section(self):
        # The consequence the report is about: without the isinstance guard this
        # raises AttributeError: 'str' object has no attribute 'get'.
        module = import_plugin_config(self, "just-a-scalar")

        config = module.load_config()

        self.assertEqual(config.workspace, "")
        self.assertFalse(config.auto_checkpoint)

    def test_a_mapping_section_still_reaches_load_config(self):
        # The guard that keeps the two tests above honest: if the stub ever
        # stopped taking effect, `_read_yaml_config` would fall into its
        # `except Exception: return {}` and both would pass for the wrong reason.
        module = import_plugin_config(
            self, {"workspace": "/tmp/project", "autoCheckpoint": True}
        )

        self.assertEqual(module._read_yaml_config(),
                         {"workspace": "/tmp/project", "autoCheckpoint": True})
        self.assertEqual(module.load_config().workspace, "/tmp/project")
        self.assertTrue(module.load_config().auto_checkpoint)


if __name__ == "__main__":
    unittest.main()
