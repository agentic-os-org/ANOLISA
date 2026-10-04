"""HTTP failures from the compatible image endpoint keep their meaning."""

import contextlib
import importlib.util
import io
from pathlib import Path
from unittest.mock import patch
from urllib.error import HTTPError


def _load_generator():
    spec = importlib.util.spec_from_file_location(
        "generate_image", Path(__file__).with_name("generate_image.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _http_error(code):
    return HTTPError(
        "https://example.invalid/images/generations",
        code,
        "request failed",
        {},
        io.BytesIO(b'{"error":"invalid API key"}'),
    )


def test_authentication_error_is_reported_without_wanx_fallback():
    generator = _load_generator()
    stderr = io.StringIO()
    with patch.object(generator.urllib.request, "urlopen", side_effect=_http_error(401)):
        with patch.object(generator, "_wanx") as wanx:
            with contextlib.redirect_stderr(stderr):
                try:
                    generator._compat("prompt", "custom-model", "1024*1024", "key", "https://example.invalid")
                except SystemExit as exc:
                    assert exc.code == 1
                else:
                    raise AssertionError("HTTP 401 should stop image generation")
            wanx.assert_not_called()
    assert "HTTP 401" in stderr.getvalue()
    assert "invalid API key" in stderr.getvalue()


def test_missing_compatible_endpoint_can_still_fallback():
    generator = _load_generator()
    with patch.object(generator.urllib.request, "urlopen", side_effect=_http_error(404)):
        with patch.object(generator, "_wanx", return_value="fallback-image") as wanx:
            result = generator._compat("prompt", "custom-model", "1024*1024", "key", "https://example.invalid")
    assert result == "fallback-image"
    wanx.assert_called_once_with("prompt", "custom-model", "1024*1024", "key")
