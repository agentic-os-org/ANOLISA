#!/usr/bin/env python3
"""Regression tests for the provider-key lookup in generate_image.py (#3628).

``_key()`` read ``~/.openclaw/openclaw.json`` and returned the FIRST
``apiKey`` it found while iterating every provider, although both lookup
comments documented ``providers.dashscope.apiKey``. With a multi-provider
config the script silently picked e.g. the OpenAI key and sent it to
``https://dashscope.aliyuncs.com``, producing a 401 whose message points
nowhere near the real cause. The lookup is now by provider name in both
config formats; env-var precedence above it is unchanged.

Stdlib only (generate_image.py has no third-party imports); no network.

Baseline on unchanged main: the two multi-provider tests fail with
``sk-OPENAI`` instead of the dashscope key.
"""

import contextlib
import importlib.util
import io
import json
import os
import tempfile
import unittest
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "others"
    / "image-gen"
    / "scripts"
    / "generate_image.py"
)

ENV_VARS = ("DASHSCOPE_API_KEY", "QWEN_API_KEY", "OPENAI_API_KEY")

# ``openai`` is deliberately listed before ``dashscope``: dict order is what
# made the first-match lookup return the wrong key.
MULTI_PROVIDER = {
    "providers": {
        "openai": {"apiKey": "sk-OPENAI"},
        "dashscope": {"apiKey": "sk-DASH"},
    }
}
MULTI_PROVIDER_NESTED = {"models": MULTI_PROVIDER}


def load_module():
    spec = importlib.util.spec_from_file_location("generate_image_under_test", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class KeyLookupTestCase(unittest.TestCase):
    """Drives the real ``_key()`` against a crafted config in a temp HOME."""

    @classmethod
    def setUpClass(cls):
        cls.mod = load_module()

    def setUp(self):
        self._saved_env = {k: os.environ.pop(k, None) for k in ENV_VARS}
        self._saved_home = os.environ.get("HOME")
        self._tmp = tempfile.TemporaryDirectory()
        os.environ["HOME"] = self._tmp.name

    def tearDown(self):
        self._tmp.cleanup()
        if self._saved_home is None:
            os.environ.pop("HOME", None)
        else:
            os.environ["HOME"] = self._saved_home
        for k, v in self._saved_env.items():
            if v is not None:
                os.environ[k] = v

    def write_config(self, content):
        cfg_dir = Path(self._tmp.name) / ".openclaw"
        cfg_dir.mkdir(parents=True, exist_ok=True)
        (cfg_dir / "openclaw.json").write_text(
            content if isinstance(content, str) else json.dumps(content)
        )

    def key(self):
        return self.mod._key()

    def assert_clean_key_error(self):
        """No usable dashscope key: actionable error and exit 1, never a guess."""
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            with self.assertRaises(SystemExit) as cm:
                self.key()
        self.assertEqual(cm.exception.code, 1)
        self.assertIn("No API key", err.getvalue())


class TestDashscopeKeySelectedByName(KeyLookupTestCase):
    def test_first_format_skips_earlier_provider(self):
        self.write_config(MULTI_PROVIDER)
        self.assertEqual(self.key(), "sk-DASH")

    def test_nested_models_format_skips_earlier_provider(self):
        self.write_config(MULTI_PROVIDER_NESTED)
        self.assertEqual(self.key(), "sk-DASH")

    def test_first_format_prefers_top_level_over_nested(self):
        nested = {
            "providers": {"dashscope": {"apiKey": "sk-TOP"}},
            "models": {"providers": {"dashscope": {"apiKey": "sk-NESTED"}}},
        }
        self.write_config(nested)
        self.assertEqual(self.key(), "sk-TOP")

    def test_nested_format_used_when_top_level_has_no_dashscope(self):
        cfg = {
            "providers": {"openai": {"apiKey": "sk-OPENAI"}},
            "models": {"providers": {"dashscope": {"apiKey": "sk-NESTED"}}},
        }
        self.write_config(cfg)
        self.assertEqual(self.key(), "sk-NESTED")

    def test_sole_dashscope_provider_still_works(self):
        self.write_config({"providers": {"dashscope": {"apiKey": "sk-ONLY"}}})
        self.assertEqual(self.key(), "sk-ONLY")


class TestEnvPrecedenceUnchanged(KeyLookupTestCase):
    def test_dashscope_env_wins_over_config(self):
        self.write_config(MULTI_PROVIDER)
        os.environ["DASHSCOPE_API_KEY"] = "sk-ENV"
        self.assertEqual(self.key(), "sk-ENV")

    def test_openai_env_still_wins_over_config(self):
        """Env order above the file lookup is part of the documented contract."""
        self.write_config(MULTI_PROVIDER)
        os.environ["OPENAI_API_KEY"] = "sk-ENV-OPENAI"
        self.assertEqual(self.key(), "sk-ENV-OPENAI")

    def test_empty_env_falls_through_to_config(self):
        self.write_config(MULTI_PROVIDER)
        os.environ["DASHSCOPE_API_KEY"] = ""
        self.assertEqual(self.key(), "sk-DASH")


class TestNoDashscopeKeyExitsCleanly(KeyLookupTestCase):
    def test_only_other_providers_configured(self):
        self.write_config({"providers": {"openai": {"apiKey": "sk-OPENAI"}}})
        self.assert_clean_key_error()

    def test_nested_only_other_providers_configured(self):
        self.write_config({"models": {"providers": {"openai": {"apiKey": "sk-OPENAI"}}}})
        self.assert_clean_key_error()

    def test_dashscope_entry_without_apikey(self):
        cfg = {
            "providers": {
                "openai": {"apiKey": "sk-OPENAI"},
                "dashscope": {"baseUrl": "https://dashscope.aliyuncs.com"},
            }
        }
        self.write_config(cfg)
        self.assert_clean_key_error()

    def test_dashscope_entry_with_empty_apikey(self):
        cfg = {"providers": {"openai": {"apiKey": "sk-OPENAI"}, "dashscope": {"apiKey": ""}}}
        self.write_config(cfg)
        self.assert_clean_key_error()

    def test_dashscope_entry_is_not_an_object(self):
        self.write_config({"providers": {"openai": {"apiKey": "sk-OPENAI"}, "dashscope": "sk-STR"}})
        self.assert_clean_key_error()

    def test_providers_section_is_null(self):
        self.write_config('{"providers": null}')
        self.assert_clean_key_error()

    def test_malformed_json(self):
        self.write_config('{"providers": ')
        self.assert_clean_key_error()

    def test_missing_config_file(self):
        self.assert_clean_key_error()


if __name__ == "__main__":
    unittest.main(verbosity=2)
