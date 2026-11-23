#!/usr/bin/env python3
"""Unit tests for hook_dispatch.resolve_agent_id attribution precedence.

The agent id must be resolvable via a robust ``--agent-id`` command argument
(the host always honours the hook ``command`` string), fall back to the
``TOKENLESS_AGENT_ID`` env var, and surface a visible ``"unknown"`` sentinel —
never the tool's own name — when neither is present, so an attribution gap is
observable in stats instead of masquerading as a real ``tokenless`` agent.
"""

import os
import sys
import unittest
from pathlib import Path

_HOOKS_DIR = (
    Path(__file__).resolve().parent.parent
    / "adapters"
    / "tokenless"
    / "common"
    / "hooks"
)

sys.path.insert(0, str(_HOOKS_DIR))
sys.path.insert(0, str(_HOOKS_DIR.parent.parent))
import hook_dispatch



class ResolveAgentIdTest(unittest.TestCase):
    def setUp(self) -> None:
        self._saved_env = os.environ.copy()
        # Host-owned signals must not leak in from the caller's environment.
        for key in ("TOKENLESS_AGENT_ID", "COSH_RUNTIME", "COSH_NG_VERSION"):
            os.environ.pop(key, None)

    def tearDown(self) -> None:
        os.environ.clear()
        os.environ.update(self._saved_env)

    def test_missing_signals_yield_visible_unknown(self) -> None:
        self.assertEqual(hook_dispatch.resolve_agent_id(argv=[]), "unknown")

    def test_env_used_when_no_argument(self) -> None:
        os.environ["TOKENLESS_AGENT_ID"] = "claude-code"
        self.assertEqual(hook_dispatch.resolve_agent_id(argv=[]), "claude-code")

    def test_argument_wins_over_env(self) -> None:
        os.environ["TOKENLESS_AGENT_ID"] = "claude-code"
        self.assertEqual(
            hook_dispatch.resolve_agent_id(argv=["--agent-id", "qoder-cli"]),
            "qoder-cli",
        )

    def test_argument_equals_form(self) -> None:
        self.assertEqual(
            hook_dispatch.resolve_agent_id(argv=["--agent-id=qoder-cli"]),
            "qoder-cli",
        )

    def test_qwenwork_argument_overrides_inherited_agent(self) -> None:
        os.environ["TOKENLESS_AGENT_ID"] = "qoder-cli"
        for args in (["--agent-id", "qwenwork"], ["--agent-id=qwenwork"]):
            with self.subTest(args=args):
                self.assertEqual(hook_dispatch.resolve_agent_id(argv=args), "qwenwork")

    def test_blank_argument_falls_through_to_env(self) -> None:
        os.environ["TOKENLESS_AGENT_ID"] = "claude-code"
        self.assertEqual(
            hook_dispatch.resolve_agent_id(argv=["--agent-id", ""]),
            "claude-code",
        )

    def test_cosh_ng_runtime_wins_over_argument_and_env(self) -> None:
        os.environ["TOKENLESS_AGENT_ID"] = "copilot-shell"
        os.environ["COSH_RUNTIME"] = "cosh-ng"
        self.assertEqual(
            hook_dispatch.resolve_agent_id(argv=["--agent-id", "qoder-cli"]),
            "cosh-ng",
        )

    def test_reads_process_argv_by_default(self) -> None:
        saved = sys.argv
        try:
            sys.argv = ["compress_response_hook.py", "--agent-id", "qoder-cli"]
            self.assertEqual(hook_dispatch.resolve_agent_id(), "qoder-cli")
        finally:
            sys.argv = saved


if __name__ == "__main__":
    unittest.main()
