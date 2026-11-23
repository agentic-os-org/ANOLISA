"""Direct PermissionDecision construction at the three example/fixture sites."""

from __future__ import annotations
import asyncio
import importlib
import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

from agentscope import tool as agentscope_tool
from agentscope.permission import PermissionBehavior

from middleware_support import ROOT


class ToolkitCaptured(Exception):
    """Stop the cancellation worker right after its LocalTool is constructed."""


def _load_module(name: str, path: Path) -> Any:
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


class PermissionDecisionTests(unittest.IsolatedAsyncioTestCase):
    """Await each example/fixture check_permissions against the pinned SDK.

    The demo and native runs declare read-only tools, so the SDK permission
    engine allows them on its fast path before check_permissions is reached;
    only a direct call covers these PermissionDecision constructors.
    """

    @classmethod
    def setUpClass(cls) -> None:
        for entry in [
            ROOT / "crates/aw-adapter/native/agentscope",
            ROOT / "crates/aw-cli/tests",
        ]:
            if str(entry) not in sys.path:
                sys.path.append(str(entry))
        # worker.py resolves its scratch root at import time and constructs
        # AwBridge before its toolkit, so provide the inputs both need; the
        # tool side effects that would use them never run in this suite.
        cls.scratch = tempfile.TemporaryDirectory(prefix="aw-permissions-")
        config_path = Path(cls.scratch.name) / "native-config.json"
        config_path.write_text(
            json.dumps(
                {
                    "executable": "aw",
                    "binding": str(Path(cls.scratch.name) / "binding.json"),
                    "ready_path": str(Path(cls.scratch.name) / "ready.json"),
                    "ready_token": "permissions",
                    "hooks": [
                        {
                            "event": "tool.before",
                            "step": "first",
                            "on_error": "report",
                            "replace_result": False,
                            "budget_ms": 1000,
                        }
                    ],
                }
            )
        )
        for key, value in [
            ("AW_TEST_ROOT", cls.scratch.name),
            ("AW_TEST_BINARY", "aw"),
            ("AW_NATIVE_CONFIG", str(config_path)),
        ]:
            os.environ.setdefault(key, value)

        demo = _load_module(
            "agentscope_demo", ROOT / "crates/aw-service/examples/agentscope_demo.py"
        )
        worker = importlib.import_module("fixtures.agentscope.worker")
        cancellation = _load_module(
            "agentscope_cancellation", ROOT / "crates/aw-cli/tests/agentscope_cancellation.py"
        )
        captured: list[Any] = []

        def capture_toolkit(*, tools: list[Any]) -> None:
            captured.extend(tools)
            raise ToolkitCaptured()

        with patch.object(agentscope_tool, "Toolkit", capture_toolkit):
            try:
                asyncio.run(cancellation.worker())
            except ToolkitCaptured:
                pass
            else:
                raise AssertionError("cancellation worker ran past toolkit construction")
        cls.sites = [
            (demo.Echo(), {"text": "hello"}),
            (worker.LocalTool(), {"mode": "success", "uid": "session"}),
            (captured[0], {"mode": "normal"}),
        ]

    @classmethod
    def tearDownClass(cls) -> None:
        cls.scratch.cleanup()

    async def test_permission_decisions_return_allow_with_a_message(self) -> None:
        for tool, tool_input in self.sites:
            with self.subTest(module=type(tool).__module__, tool=tool.name):
                decision = await tool.check_permissions(tool_input, None)
                self.assertEqual(decision.behavior, PermissionBehavior.ALLOW)
                self.assertIsInstance(decision.message, str)
                self.assertTrue(decision.message)
