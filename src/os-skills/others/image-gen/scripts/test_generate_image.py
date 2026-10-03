#!/usr/bin/env python3
"""Tests for generate_image._key provider lookup."""
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import generate_image

ENV_VARS = ("DASHSCOPE_API_KEY", "QWEN_API_KEY", "OPENAI_API_KEY")


class KeyLookupTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        home = Path(self._tmp.name)
        (home / ".openclaw").mkdir()
        self._old_home = os.environ.get("HOME")
        self._saved_env = {v: os.environ.get(v) for v in ENV_VARS}
        os.environ["HOME"] = str(home)
        for v in ENV_VARS:
            os.environ.pop(v, None)

    def tearDown(self):
        if self._old_home is not None:
            os.environ["HOME"] = self._old_home
        for v, val in self._saved_env.items():
            if val is not None:
                os.environ[v] = val
            else:
                os.environ.pop(v, None)
        self._tmp.cleanup()

    def _write_config(self, text):
        home = Path(os.environ["HOME"])
        (home / ".openclaw" / "openclaw.json").write_text(text, encoding="utf-8")

    def test_dashscope_key_wins_over_first_provider(self):
        self._write_config(
            '{"providers":{"openai":{"apiKey":"sk-OPENAI"},'
            '"dashscope":{"apiKey":"sk-DASH"}}}'
        )
        self.assertEqual(generate_image._key(), "sk-DASH")

    def test_models_nested_dashscope_key(self):
        self._write_config(
            '{"models":{"providers":{"openai":{"apiKey":"sk-OPENAI"},'
            '"dashscope":{"apiKey":"sk-DASH2"}}}}'
        )
        self.assertEqual(generate_image._key(), "sk-DASH2")

    def test_env_var_still_takes_precedence(self):
        self._write_config('{"providers":{"dashscope":{"apiKey":"sk-DASH"}}}')
        os.environ["DASHSCOPE_API_KEY"] = "sk-ENV"
        self.assertEqual(generate_image._key(), "sk-ENV")

    def test_missing_dashscope_entry_exits_cleanly(self):
        self._write_config('{"providers":{"openai":{"apiKey":"sk-OPENAI"}}}')
        with self.assertRaises(SystemExit):
            generate_image._key()


if __name__ == "__main__":
    unittest.main()
