#!/usr/bin/env python3
"""Golden parity for the Common Hook Protocol v2 transport.

Replays the tests/contract corpus through the current hooks against the
real `tokenless` debug binary and compares every envelope with the Protocol
v2 goldens. The baseline includes Marker-command recovery policy for Common
Hooks and JSON/Build Log PostTool dispatch.

Requires target/debug/tokenless — run `cargo build -p tokenless-cli` first
(the Makefile target does).
"""

import json
import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "contract"))

import corpus


@unittest.skipUnless(
    os.path.exists(corpus.DEBUG_TOKENLESS_BIN),
    "tokenless debug binary not built",
)
class HookParity(unittest.TestCase):
    maxDiff = None

    def run_matrix(self, kind, hook, agents):
        for name in corpus.fixture_names(kind):
            with open(corpus.fixture_path(kind, name)) as f:
                stdin_text = f.read()
            for agent, env in corpus.agents_for(kind, name, agents).items():
                with self.subTest(fixture=f"{kind}/{name}", agent=agent):
                    proc = corpus.run_hook(hook, stdin_text, env)
                    self.assertEqual(
                        proc.returncode, 0, f"hook failed: {proc.stderr}"
                    )
                    actual = json.loads(proc.stdout)
                    with open(corpus.golden_path(kind, name, agent)) as f:
                        expected = json.load(f)["envelope"]
                    self.assertEqual(actual, expected)

    def test_response_hook_matches_the_goldens(self):
        self.run_matrix("post_tool", corpus.RESPONSE_HOOK, corpus.RESPONSE_AGENTS)

    def test_schema_hook_matches_the_goldens(self):
        self.run_matrix("before_model", corpus.SCHEMA_HOOK, corpus.SCHEMA_AGENTS)


if __name__ == "__main__":
    unittest.main()
