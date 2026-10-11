#!/usr/bin/env python3
"""Regression tests for generate_image.py API-key discovery.

``~/.openclaw/openclaw.json`` may legally carry ``"models": null`` (or a
list): the key lookup called .get() on it and crashed with an
AttributeError traceback. A malformed config must fall through to the
actionable "No API key" error with exit code 1, not a traceback.
"""

import importlib.util
import os
import unittest
from unittest import mock

SCRIPT = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..", "..", "src", "os-skills", "others", "image-gen", "scripts",
    "generate_image.py",
)


def _load_module():
    spec = importlib.util.spec_from_file_location("genimg_test", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class TestKeyLookupConfigShapes(unittest.TestCase):
    def setUp(self):
        self.mod = _load_module()
        self._saved = {
            k: os.environ.pop(k, None)
            for k in ("DASHSCOPE_API_KEY", "QWEN_API_KEY", "OPENAI_API_KEY")
        }

    def tearDown(self):
        for k, v in self._saved.items():
            if v is not None:
                os.environ[k] = v

    def _assert_clean_key_error(self, config_content, tmpdir):
        cfg = os.path.join(tmpdir, "openclaw.json")
        with open(cfg, "w") as f:
            f.write(config_content)
        with mock.patch("os.path.expanduser", return_value=cfg):
            with self.assertRaises(SystemExit) as cm:
                self.mod._key()
        self.assertEqual(cm.exception.code, 1)

    def test_null_models_config_reports_clean_error(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmpdir:
            self._assert_clean_key_error('{"models": null}', tmpdir)

    def test_list_models_config_reports_clean_error(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmpdir:
            self._assert_clean_key_error('{"models": []}', tmpdir)

    def test_null_providers_still_reports_clean_error(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmpdir:
            self._assert_clean_key_error('{"models": {"providers": null}}', tmpdir)

    def test_nested_models_provider_key_is_found(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmpdir:
            cfg = os.path.join(tmpdir, "openclaw.json")
            with open(cfg, "w") as f:
                f.write('{"models": {"providers": {"dashscope": {"apiKey": "sk-x"}}}}')
            with mock.patch("os.path.expanduser", return_value=cfg):
                self.assertEqual(self.mod._key(), "sk-x")


if __name__ == "__main__":
    unittest.main()
