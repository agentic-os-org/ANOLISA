"""Unit tests for model_service package (OllamaClient, create_client).

All HTTP calls are mocked — no real Ollama instance is required.
"""

import json
import os
import unittest
from unittest.mock import MagicMock, patch

from agent_sec_cli.model_service import create_client
from agent_sec_cli.model_service.ollama import OllamaClient

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _mock_urlopen(response_data: dict | None = None, error: Exception | None = None):
    """Create a mock for urllib.request.urlopen.

    If *error* is provided, the mock raises it.  Otherwise it returns a
    context manager whose ``read()`` returns the JSON-encoded *response_data*.
    """
    cm = MagicMock()
    if error is not None:
        cm.side_effect = error
    else:
        payload = json.dumps(response_data or {}).encode("utf-8")
        cm.return_value.__enter__.return_value.read.return_value = payload
    return cm


# ---------------------------------------------------------------------------
# OllamaClient
# ---------------------------------------------------------------------------


class TestOllamaClientInit(unittest.TestCase):
    def test_strips_trailing_slash_from_base_url(self) -> None:
        client = OllamaClient(base_url="http://localhost:11434/")
        self.assertEqual(client._base_url, "http://localhost:11434")

    def test_keeps_base_url_without_trailing_slash(self) -> None:
        client = OllamaClient(base_url="http://localhost:11434")
        self.assertEqual(client._base_url, "http://localhost:11434")

    def test_default_timeout(self) -> None:
        client = OllamaClient()
        self.assertEqual(client._timeout, 30)


class TestOllamaClientCheckModel(unittest.TestCase):
    def test_model_found_exact_match(self) -> None:
        client = OllamaClient()
        response = {"models": [{"name": "warden"}]}
        with patch("urllib.request.urlopen", _mock_urlopen(response)):
            self.assertTrue(client.check_model("warden"))

    def test_model_found_with_tag(self) -> None:
        """'warden' should match 'warden:latest' (name:tag prefix)."""
        client = OllamaClient()
        response = {"models": [{"name": "warden:latest"}]}
        with patch("urllib.request.urlopen", _mock_urlopen(response)):
            self.assertTrue(client.check_model("warden"))

    def test_model_not_found(self) -> None:
        client = OllamaClient()
        response = {"models": [{"name": "llama2"}, {"name": "mistral"}]}
        with patch("urllib.request.urlopen", _mock_urlopen(response)):
            self.assertFalse(client.check_model("warden"))

    def test_substring_does_not_match(self) -> None:
        """'war' must NOT match 'warden-tmp' (substring matching disabled)."""
        client = OllamaClient()
        response = {"models": [{"name": "warden-tmp"}]}
        with patch("urllib.request.urlopen", _mock_urlopen(response)):
            self.assertFalse(client.check_model("war"))

    def test_empty_model_list(self) -> None:
        client = OllamaClient()
        response = {"models": []}
        with patch("urllib.request.urlopen", _mock_urlopen(response)):
            self.assertFalse(client.check_model("warden"))

    def test_network_error_returns_false(self) -> None:
        """check_model should return False (not raise) on network errors."""
        import urllib.error

        client = OllamaClient()
        err = urllib.error.URLError("Connection refused")
        with patch("urllib.request.urlopen", _mock_urlopen(error=err)):
            self.assertFalse(client.check_model("warden"))

    def test_json_decode_error_returns_false(self) -> None:
        """check_model should return False on malformed JSON."""
        client = OllamaClient()
        cm = MagicMock()
        cm.return_value.__enter__.return_value.read.return_value = b"not json"
        with patch("urllib.request.urlopen", cm):
            self.assertFalse(client.check_model("warden"))


class TestOllamaClientGenerate(unittest.TestCase):
    def test_generate_success(self) -> None:
        client = OllamaClient()
        response = {"response": "1", "logprobs": []}
        with patch("urllib.request.urlopen", _mock_urlopen(response)) as mock_urlopen:
            result = client.generate("warden", "test prompt")
            self.assertEqual(result["response"], "1")
            # Verify the request was constructed correctly
            call_args = mock_urlopen.call_args
            req = call_args[0][0]
            self.assertEqual(req.method, "POST")
            self.assertIn("/api/generate", req.full_url)

    def test_generate_with_logprobs(self) -> None:
        client = OllamaClient()
        response = {"response": "0", "logprobs": [{"top_logprobs": []}]}
        with patch("urllib.request.urlopen", _mock_urlopen(response)):
            result = client.generate(
                "warden",
                "prompt",
                logprobs=True,
                top_logprobs=10,
            )
            self.assertIn("logprobs", result)

    def test_generate_with_options(self) -> None:
        client = OllamaClient()
        response = {"response": "1"}
        with patch("urllib.request.urlopen", _mock_urlopen(response)) as mock_urlopen:
            client.generate(
                "warden",
                "prompt",
                options={"num_predict": 1, "temperature": 0},
            )
            req = mock_urlopen.call_args[0][0]
            payload = json.loads(req.data)
            self.assertEqual(payload["options"]["num_predict"], 1)
            self.assertEqual(payload["options"]["temperature"], 0)

    def test_generate_url_error_raises_runtime_error(self) -> None:
        import urllib.error

        client = OllamaClient()
        err = urllib.error.URLError("Connection refused")
        with patch("urllib.request.urlopen", _mock_urlopen(error=err)):
            with self.assertRaises(RuntimeError) as ctx:
                client.generate("warden", "prompt")
            self.assertIn("Ollama request failed", str(ctx.exception))

    def test_generate_generic_error_raises_runtime_error(self) -> None:
        client = OllamaClient()
        with patch("urllib.request.urlopen", _mock_urlopen(error=ValueError("bad"))):
            with self.assertRaises(RuntimeError) as ctx:
                client.generate("warden", "prompt")
            self.assertIn("Ollama request error", str(ctx.exception))


# ---------------------------------------------------------------------------
# create_client factory
# ---------------------------------------------------------------------------


class TestCreateClient(unittest.TestCase):
    def test_default_returns_ollama_client(self) -> None:
        client = create_client()
        self.assertIsInstance(client, OllamaClient)

    def test_explicit_ollama_backend(self) -> None:
        client = create_client(backend="ollama")
        self.assertIsInstance(client, OllamaClient)

    def test_custom_base_url(self) -> None:
        client = create_client(base_url="http://my-host:1234")
        self.assertIsInstance(client, OllamaClient)
        self.assertEqual(client._base_url, "http://my-host:1234")

    def test_custom_timeout(self) -> None:
        client = create_client(timeout=60)
        self.assertEqual(client._timeout, 60)

    def test_timeout_zero_is_preserved(self) -> None:
        # Regression: ``timeout=0`` (disable timeout) must not be swallowed by a
        # truthy fallback to env var / default.
        with patch.dict(os.environ, {"AGENT_SEC_MODEL_SERVICE_TIMEOUT": "45"}):
            client = create_client(timeout=0)
            self.assertEqual(client._timeout, 0)

    def test_unsupported_backend_raises(self) -> None:
        with self.assertRaises(ValueError) as ctx:
            create_client(backend="vllm")
        self.assertIn("Unsupported model service backend", str(ctx.exception))

    def test_env_var_backend(self) -> None:
        with patch.dict(os.environ, {"AGENT_SEC_MODEL_SERVICE_BACKEND": "ollama"}):
            client = create_client()
            self.assertIsInstance(client, OllamaClient)

    def test_env_var_base_url(self) -> None:
        with patch.dict(
            os.environ,
            {"AGENT_SEC_MODEL_SERVICE_BASE_URL": "http://127.0.0.1:9999"},
        ):
            client = create_client()
            self.assertEqual(client._base_url, "http://127.0.0.1:9999")

    def test_env_var_non_loopback_base_url_rejected(self) -> None:
        # Regression: a hijacked env var must not silently ship scanned content
        # (which may carry credentials/PII) to an arbitrary host.
        for remote in (
            "http://env:9999",
            "https://model.internal:8443",
            "http://10.0.0.5:18099",
        ):
            with patch.dict(
                os.environ,
                {"AGENT_SEC_MODEL_SERVICE_BASE_URL": remote},
            ):
                with self.assertRaises(ValueError) as ctx:
                    create_client()
                self.assertIn(remote, str(ctx.exception))

    def test_env_var_base_url_rejects_non_http_scheme(self) -> None:
        for bad in ("ftp://localhost:11434", "file:///etc/passwd", "localhost:11434"):
            with patch.dict(
                os.environ,
                {"AGENT_SEC_MODEL_SERVICE_BASE_URL": bad},
            ):
                with self.assertRaises(ValueError):
                    create_client()

    def test_explicit_base_url_bypasses_env_validation(self) -> None:
        # Programmatic callers are trusted, mirroring the Rust crate where
        # OllamaClient::new() is likewise unvalidated; only env-derived values
        # are attacker-influenced.
        client = create_client(base_url="http://explicit.remote:8080")
        self.assertEqual(client._base_url, "http://explicit.remote:8080")

    def test_env_var_timeout(self) -> None:
        with patch.dict(os.environ, {"AGENT_SEC_MODEL_SERVICE_TIMEOUT": "45"}):
            client = create_client()
            self.assertEqual(client._timeout, 45)

    def test_env_var_timeout_invalid_falls_back(self) -> None:
        with patch.dict(
            os.environ, {"AGENT_SEC_MODEL_SERVICE_TIMEOUT": "not-a-number"}
        ):
            client = create_client()
            self.assertEqual(client._timeout, 30)

    def test_explicit_args_override_env_vars(self) -> None:
        with patch.dict(
            os.environ,
            {
                "AGENT_SEC_MODEL_SERVICE_BASE_URL": "http://env:9999",
                "AGENT_SEC_MODEL_SERVICE_TIMEOUT": "45",
            },
        ):
            client = create_client(base_url="http://explicit:8080", timeout=10)
            self.assertEqual(client._base_url, "http://explicit:8080")
            self.assertEqual(client._timeout, 10)


if __name__ == "__main__":
    unittest.main()
