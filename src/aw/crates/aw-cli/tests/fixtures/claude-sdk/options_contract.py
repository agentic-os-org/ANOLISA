"""Exercise public option composition without an installed SDK or native CLI."""

from dataclasses import dataclass, fields
from pathlib import Path
import sys
import tempfile
from types import ModuleType
import unittest

sys.path.insert(0, sys.argv.pop(1))


@dataclass
class Options:
    setting_sources: object = None
    settings: object = None
    plugins: object = None
    mcp_servers: object = None
    hooks: object = None
    can_use_tool: object = None
    permission_prompt_tool_name: object = None
    extra_args: object = None
    cli_path: object = None
    cwd: object = None
    env: object = None


@dataclass
class Matcher:
    hooks: object
    timeout: object = None


sdk = ModuleType("claude_agent_sdk")
sdk.ClaudeAgentOptions, sdk.HookMatcher = Options, Matcher
sdk.ClaudeSDKClient = sdk.PermissionResultDeny = object
sys.modules["claude_agent_sdk"] = sdk

from aw_claude import Bridge


class Contracts(unittest.TestCase):
    def test_native_sources_settings_plugins_and_mcp_are_retained(self):
        with tempfile.TemporaryDirectory() as directory:
            sdk.__file__ = str(Path(directory) / "__init__.py")
            bundled = Path(directory) / "_bundled/claude"
            bundled.parent.mkdir()
            bundled.touch()
            for sources, settings in ((None, None), (["user", "project", "local"],
                                      '{"env":{"REFERENCE":"${TOKEN}"},"future":[1]}'),
                                     (["project"], "./relative-settings.json")):
                with self.subTest(sources=sources, settings=settings):
                    native = object()
                    hooks = {"SessionStart": [Matcher(hooks=[native])]}
                    options = Options(
                        setting_sources=sources, settings=settings,
                        plugins=[{"type": "local", "path": "./plugin"}],
                        mcp_servers={"remote": {"type": "http", "url": "https://fixture.invalid/mcp",
                                               "headers": {"X-Context": "${TOKEN}"}}},
                        hooks=hooks, extra_args={}, cwd="./business",
                        env={"BUSINESS_CONFIG": "./config.json"},
                    )
                    bridge = object.__new__(Bridge)
                    bridge.config = {"hooks": [{"event": "tool.before", "budget_ms": 1000,
                                                "replace_result": False}]}
                    composed = bridge.options(options)
                    for field in fields(options):
                        if field.name not in {"hooks", "cli_path"}:
                            self.assertIs(getattr(composed, field.name), getattr(options, field.name))
                    self.assertIs(composed.hooks["SessionStart"][0], hooks["SessionStart"][0])
                    self.assertEqual(composed.hooks["PreToolUse"][0].hooks, [bridge.callback])
                    self.assertEqual(list(hooks), ["SessionStart"])
                    self.assertIsNone(options.cli_path)
                    self.assertEqual(composed.cli_path, str(bundled))


if __name__ == "__main__":
    unittest.main()
