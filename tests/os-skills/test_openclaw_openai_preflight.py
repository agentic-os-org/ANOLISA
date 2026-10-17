"""Validate configured OpenAI-compatible endpoints before installation or config writes."""

import contextlib
import importlib.util
import io
import json
import unittest
import urllib.error
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "src/os-skills/ai/install-openclaw/scripts/install_openclaw.py"


class OpenAICompatiblePreflightTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("openclaw_openai_preflight", SCRIPT)
        cls.installer = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.installer)

    def setUp(self):
        self.args = SimpleNamespace(skip_preflight=False, preflight_timeout=17)
        self.metadata = {
            "api": "openai-completions",
            "base_url": "https://example.invalid/compatible-mode/v1",
            "api_key": "test-key-not-secret",
            "model_id": "model-sample",
            "billing": "payg",
            "api_key_url": "https://example.invalid/key-help",
            "model_catalog_url": "https://example.invalid/models",
        }

    def preflight(self, **patch_options):
        response = mock.Mock()
        response.__enter__ = mock.Mock(return_value=response)
        response.__exit__ = mock.Mock(return_value=False)
        response.read.return_value = b'{"choices":[]}'
        if not patch_options:
            patch_options = {"return_value": response}
        output = io.StringIO()
        with (
            mock.patch.object(self.installer.urllib.request, "urlopen", **patch_options) as send,
            contextlib.redirect_stdout(output),
        ):
            self.installer.preflight_model_call(self.args, self.metadata)
        return send, output.getvalue()

    def test_compatible_request_uses_chat_endpoint_and_bearer_auth(self):
        send, output = self.preflight()
        send.assert_called_once()
        request = send.call_args.args[0]
        self.assertEqual(
            request.full_url, "https://example.invalid/compatible-mode/v1/chat/completions"
        )
        self.assertEqual(request.get_method(), "POST")
        self.assertEqual(request.get_header("Authorization"), "Bearer test-key-not-secret")
        self.assertIsNone(request.get_header("X-api-key"))
        self.assertIsNone(request.get_header("Anthropic-version"))
        self.assertEqual(
            json.loads(request.data),
            {
                "model": "model-sample",
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "ping"}],
            },
        )
        self.assertEqual(send.call_args.kwargs["timeout"], 17)
        self.assertIn("[OK]", output)
        self.assertNotIn(self.metadata["api_key"], output)

    def test_trailing_slash_and_full_endpoint_do_not_duplicate_path(self):
        for base in (
            "https://example.invalid/v1/",
            "https://example.invalid/v1/chat/completions",
            "https://example.invalid/v1/chat/completions/",
        ):
            with self.subTest(base=base):
                self.metadata["base_url"] = base
                send, _ = self.preflight()
                send.assert_called_once()
                self.assertEqual(
                    send.call_args.args[0].full_url, "https://example.invalid/v1/chat/completions"
                )

    def test_endpoint_query_parameters_remain_after_the_chat_path(self):
        self.metadata["base_url"] = "https://example.invalid/custom/v1/?api-version=preview"
        send, _ = self.preflight()
        self.assertEqual(
            send.call_args.args[0].full_url,
            "https://example.invalid/custom/v1/chat/completions?api-version=preview",
        )

    def test_http_failure_reuses_existing_provider_diagnostics(self):
        error = urllib.error.HTTPError(
            "https://example.invalid/v1/chat/completions",
            401,
            "Unauthorized",
            {},
            io.BytesIO(b'{"error":{"message":"Invalid API key"}}'),
        )
        self.addCleanup(error.close)
        with self.assertRaises(SystemExit) as failure:
            self.preflight(side_effect=error)
        message = str(failure.exception)
        self.assertIn("before writing OpenClaw config", message)
        self.assertIn("HTTP 401", message)
        self.assertIn("Invalid API key", message)
        self.assertIn("Authentication failed", message)
        self.assertNotIn(self.metadata["api_key"], message)

    def test_network_failure_reuses_existing_clean_failure(self):
        with self.assertRaises(SystemExit) as failure:
            self.preflight(side_effect=urllib.error.URLError("unreachable endpoint"))
        self.assertIn("unreachable endpoint", str(failure.exception))
        self.assertIn("--skip-preflight", str(failure.exception))

    def test_explicit_skip_still_sends_no_request(self):
        self.args.skip_preflight = True
        send, output = self.preflight()
        send.assert_not_called()
        self.assertIn("--skip-preflight", output)

    def test_anthropic_request_keeps_existing_protocol(self):
        self.metadata["api"] = "anthropic-messages"
        self.metadata["base_url"] = "https://example.invalid/apps/anthropic"
        send, _ = self.preflight()
        request = send.call_args.args[0]
        self.assertEqual(request.full_url, "https://example.invalid/apps/anthropic/v1/messages")
        self.assertEqual(request.get_header("X-api-key"), "test-key-not-secret")
        self.assertEqual(request.get_header("Anthropic-version"), "2023-06-01")
        self.assertIsNone(request.get_header("Authorization"))

    def test_unimplemented_api_retains_explicit_skip_diagnostic(self):
        self.metadata["api"] = "unsupported-test-api"
        send, output = self.preflight()
        send.assert_not_called()
        self.assertIn("unsupported provider api", output)


if __name__ == "__main__":
    unittest.main()
