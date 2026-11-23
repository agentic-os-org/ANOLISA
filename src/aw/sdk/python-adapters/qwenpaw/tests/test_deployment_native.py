"""Exercise the packaged QwenPaw entrypoint with native DTO stand-ins."""

from copy import deepcopy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import patch

import os
from aw_sdk import AwError
from test_qwenpaw import Response

MULTI_ADAPTER = json.loads(
    (Path(__file__).resolve().parents[3] / "tests/fixtures/target-availability.json").read_text()
)
BINARY = os.environ["AW_TEST_BINARY"]


class NativeDeployment(unittest.IsolatedAsyncioTestCase):
    async def test_packaged_from_config_uses_descriptor_before_native_registration(self) -> None:
        modules = {}
        for name, values in {
            "agentscope": {},
            "agentscope.middleware": {"MiddlewareBase": object},
            "agentscope.tool": {"ToolResponse": Response, "ToolChunk": type("Chunk", (), {})},
            "agentscope.message": {
                "TextBlock": dict,
                "ToolResultState": SimpleNamespace(DENIED="denied"),
            },
        }.items():
            modules[name] = ModuleType(name)
            modules[name].__dict__.update(values)
        with tempfile.TemporaryDirectory(prefix="aw-plugin-descriptor-") as temporary:
            root = Path(temporary)
            config, descriptor = root / "config.json", root / "descriptor.json"
            document = deepcopy(MULTI_ADAPTER)
            config.write_text(json.dumps(document))
            config.chmod(0o600)
            result = subprocess.run(
                [
                    BINARY,
                    "config",
                    "descriptor",
                    "--config",
                    str(config),
                    "--agent",
                    "app",
                    "--profile",
                    "qwenpaw-installed-native-hooks",
                ],
                check=True,
                capture_output=True,
                timeout=10,
            )
            descriptor.write_bytes(result.stdout)
            descriptor.chmod(0o600)
            with patch.dict(sys.modules, modules):
                from aw_sdk.qwenpaw import QwenPawPlugin

                with patch(
                    "aw_sdk.configuration.subprocess.Popen",
                    side_effect=AssertionError("cold-start subprocess"),
                ):
                    plugin = QwenPawPlugin.from_config(
                        str(config),
                        descriptor_file=str(descriptor),
                        executable="/missing/aw",
                        cwd=str(root),
                    )
                self.assertTrue(plugin.input_enabled)
                self.assertFalse(plugin.registered)
                self.assertEqual(plugin.instance.target, "app")
                self.assertEqual(plugin.instance.availability, "required")
                self.assertEqual(plugin.instance.capabilities["events"]["turn.stop"], ["observe"])
                await plugin.instance.close()
                self.assertEqual(
                    {
                        key: plugin.instance.capabilities[key]
                        for key in ("adapter", "version", "entrypoint")
                    },
                    json.loads(result.stdout)["profile"],
                )
                value = json.loads(result.stdout)
                value["availability"] = "native_fallback"
                descriptor.write_text(json.dumps(value))
                with self.assertRaisesRegex(AwError, "sdk_descriptor_profile_mismatch"):
                    QwenPawPlugin.from_config(str(config), descriptor_file=str(descriptor))
                descriptor.write_bytes(result.stdout)
                document["spec"]["events"] = {}
                config.write_text(json.dumps(document))
                legacy = QwenPawPlugin.from_config(str(config), executable=BINARY, cwd=str(root))
                self.assertEqual(legacy.instance.availability, "required")
                self.assertFalse(legacy.input_enabled)
                self.assertEqual(
                    legacy.instance.capabilities,
                    {
                        **plugin.instance.capabilities,
                        "events": {
                            key: effects
                            for key, effects in plugin.instance.capabilities["events"].items()
                            if key != "input.submit"
                        },
                    },
                )
                await legacy.instance.close()
                config.write_bytes(config.read_bytes() + b"\n")
                with self.assertRaisesRegex(AwError, "sdk_descriptor_stale"):
                    QwenPawPlugin.from_config(
                        str(config),
                        descriptor_file=str(descriptor),
                        executable="/missing/aw",
                        cwd=str(root),
                    )


if __name__ == "__main__":
    unittest.main()
