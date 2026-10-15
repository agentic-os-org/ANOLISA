#!/usr/bin/env python3
"""Payload-shape contract tests for the stdin-JSON hooks.

Every hook that parses a JSON payload from stdin promises to fail open:
an error path must exit 0 with empty stdout so the agent session is never
blocked (see each hook's docstring). A payload that decodes to valid JSON
but is not an object carries no hook fields, so it belongs on that same
silent path rather than raising ``AttributeError`` and exiting 1.

``compress_schema_hook.py`` (warn + pass-through) and the codex
``response-diagnostics`` hook already handle this input class; these
tests pin the same behaviour for the remaining JSON-reading hooks.

Each test drives the real script through subprocess with a stub binary
where the hook's binary gate would otherwise skip before the parse, so
the payload-shape path itself is what is under test.
"""

import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

TESTS_DIR = Path(__file__).resolve().parent
ADAPTERS = TESTS_DIR.parent / "adapters" / "tokenless"
CODEX_SCRIPTS = ADAPTERS / "codex" / "scripts"
COMMON_HOOKS = ADAPTERS / "common" / "hooks"

TOOL_READY_SCRIPT = CODEX_SCRIPTS / "tool-ready"
CODEX_REWRITE_SCRIPT = CODEX_SCRIPTS / "rewrite-hook"
COMPRESS_RESPONSE_SCRIPT = COMMON_HOOKS / "compress_response_hook.py"
COMMON_REWRITE_SCRIPT = COMMON_HOOKS / "rewrite_hook.py"

NON_OBJECT_PAYLOADS = (
    '[{"tool_name": "Bash", "tool_input": {"command": "ls"}}]',
    '"PreToolUse"',
    "123",
    "true",
)

# Environment probes that could redirect a hook away from the code under
# test (runtime detection, agent attribution, explicit binary override).
STRIP_ENV_KEYS = (
    "TOKENLESS_BIN",
    "TOKENLESS_AGENT_ID",
    "COSH_NG_VERSION",
    "COSH_RUNTIME",
)


class HookPayloadShapeMixin:
    """Shared harness: run one hook script with a raw stdin payload.

    A mixin rather than a TestCase so pytest only collects the concrete
    per-hook classes below.
    """

    script: Path

    # The pass-through output differs by protocol: the codex hooks exit
    # with empty stdout, the common hooks print a no-op ``{}`` response
    # (hook_utils.skip) before exiting.
    expected_stdout = ""

    def setUp(self) -> None:
        self.tmpdir = tempfile.TemporaryDirectory(prefix="payload_shape_")
        self.addCleanup(self.tmpdir.cleanup)

    def build_env(self, path_dir: Path | None = None) -> dict:
        env = {k: v for k, v in os.environ.items() if k not in STRIP_ENV_KEYS}
        if path_dir is not None:
            env["PATH"] = f"{path_dir}{os.pathsep}{env.get('PATH', '')}"
        return env

    def write_stub(self, name: str, body: str) -> Path:
        stub = Path(self.tmpdir.name) / name
        stub.write_text(body)
        stub.chmod(stub.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        return stub

    def hook_env(self) -> dict:
        return self.build_env()

    def run_hook(self, payload: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(self.script)],
            input=payload,
            capture_output=True,
            text=True,
            timeout=15,
            check=False,
            env=self.hook_env(),
        )

    def test_non_object_payload_passes_through_silently(self) -> None:
        for payload in NON_OBJECT_PAYLOADS:
            with self.subTest(payload=payload):
                proc = self.run_hook(payload)
                self.assertEqual(
                    proc.returncode, 0, msg=f"stderr:\n{proc.stderr}"
                )
                self.assertEqual(
                    proc.stdout,
                    self.expected_stdout,
                    msg=f"stdout:\n{proc.stdout}",
                )


class CodexToolReadyPayloadShapeTest(HookPayloadShapeMixin, unittest.TestCase):
    """codex tool-ready must skip non-object payloads (fail-open)."""

    script = TOOL_READY_SCRIPT

    def hook_env(self) -> dict:
        # The adapter skips before reading stdin when no tokenless binary
        # resolves, so point TOKENLESS_BIN at a stub to reach the parse.
        stub = self.write_stub("tokenless", "#!/bin/sh\nexit 0\n")
        env = self.build_env()
        env["TOKENLESS_BIN"] = str(stub)
        return env


class CodexRewriteHookPayloadShapeTest(HookPayloadShapeMixin, unittest.TestCase):
    """codex rewrite-hook must skip non-object payloads (fail-open)."""

    script = CODEX_REWRITE_SCRIPT

    def hook_env(self) -> dict:
        # The version gate runs before the payload parse; satisfy it with
        # a stub rtk advertising a supported version.
        self.write_stub(
            "rtk",
            "#!/bin/sh\n"
            "if [ \"$1\" = \"--version\" ]; then echo 'rtk 0.43.0'; exit 0; fi\n"
            "exit 1\n",
        )
        return self.build_env(path_dir=Path(self.tmpdir.name))


class CompressResponsePayloadShapeTest(HookPayloadShapeMixin, unittest.TestCase):
    """common compress_response_hook must pass non-objects through."""

    script = COMPRESS_RESPONSE_SCRIPT
    expected_stdout = "{}\n"
    # No stub needed: the payload parse runs before any binary resolution.


class CommonRewritePayloadShapeTest(HookPayloadShapeMixin, unittest.TestCase):
    """common rewrite_hook must skip non-object payloads (fail-open)."""

    script = COMMON_REWRITE_SCRIPT
    expected_stdout = "{}\n"

    def hook_env(self) -> dict:
        # resolve_binary() skips the hook when tokenless is missing; a PATH
        # stub keeps the parse reachable without invoking the binary.
        self.write_stub("tokenless", "#!/bin/sh\nexit 0\n")
        return self.build_env(path_dir=Path(self.tmpdir.name))


if __name__ == "__main__":
    unittest.main()
