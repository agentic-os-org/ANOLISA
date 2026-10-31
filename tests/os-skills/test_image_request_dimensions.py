"""Check size values at the actual compatible and native HTTP boundaries."""

import importlib.util
import io
import json
import sys
import unittest
import urllib.error
from pathlib import Path
from unittest.mock import patch

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/others/image-gen/scripts/generate_image.py"
)
SPEC = importlib.util.spec_from_file_location("image_request_dimensions", SCRIPT)
GENERATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GENERATOR)


class ImageRequestDimensionsTests(unittest.TestCase):
    def requests(
        self, size: str | None, model: str, fallback: bool = False
    ) -> list[tuple[str, dict]]:
        requests = []

        def urlopen(request: object, **kwargs: object) -> io.BytesIO:
            url = request.full_url
            body = json.loads(request.data) if request.data else {}
            requests.append((url, body))
            if url.endswith("/images/generations"):
                if fallback:
                    error = urllib.error.HTTPError(url, 404, "unavailable", {}, io.BytesIO())
                    self.addCleanup(error.close)
                    raise error
                response = {"data": [{"url": "https://example.invalid/generated.png"}]}
            elif url.endswith("/image-synthesis"):
                response = {"output": {"task_id": "test-task"}}
            else:
                response = {
                    "output": {
                        "task_status": "SUCCEEDED",
                        "results": [{"url": "https://example.invalid/generated.png"}],
                    }
                }
            return io.BytesIO(json.dumps(response).encode("utf-8"))

        arguments = [str(SCRIPT), "--prompt", "sample", "--output", "unused.png", "--model", model]
        if size is not None:
            arguments += ["--size", size]
        with (
            patch.object(sys, "argv", arguments),
            patch.object(GENERATOR, "_key", return_value="test-key"),
            patch.object(GENERATOR.urllib.request, "urlopen", side_effect=urlopen),
            patch.object(GENERATOR.time, "sleep"),
            patch.object(GENERATOR, "_save") as save,
        ):
            GENERATOR.main()
            save.assert_called_once_with("https://example.invalid/generated.png", "unused.png")
        return requests

    def test_compatible_default_uses_x_separator(self) -> None:
        requests = self.requests(None, "compatible-image-model")
        self.assertEqual(requests[0][1]["size"], "1024x1024")

    def test_compatible_accepts_both_cli_separator_spellings(self) -> None:
        for size in ("1024x1536", "1024*1536"):
            with self.subTest(size=size):
                requests = self.requests(size, "compatible-image-model")
                self.assertEqual(requests[0][1]["size"], "1024x1536")

    def test_native_models_keep_star_separator(self) -> None:
        for size in (None, "1024x1024", "1024*1024"):
            with self.subTest(size=size):
                requests = self.requests(size, "wanx2.1-t2i-turbo")
                self.assertEqual(requests[0][1]["parameters"]["size"], "1024*1024")

    def test_compatible_auto_size_is_preserved(self) -> None:
        requests = self.requests("auto", "compatible-image-model")
        self.assertEqual(requests[0][1]["size"], "auto")

    def test_unavailable_compatible_route_falls_back_to_native_size(self) -> None:
        requests = self.requests("1024x1024", "compatible-image-model", fallback=True)
        self.assertEqual(requests[0][1]["size"], "1024x1024")
        self.assertEqual(requests[1][1]["parameters"]["size"], "1024*1024")

    def test_direct_compatible_call_falls_back_with_native_dimensions(self) -> None:
        requests = []

        def urlopen(request: object, **kwargs: object) -> io.BytesIO:
            body = json.loads(request.data) if request.data else {}
            requests.append(body)
            if request.full_url.endswith("/images/generations"):
                error = urllib.error.HTTPError(
                    request.full_url, 404, "unavailable", {}, io.BytesIO()
                )
                self.addCleanup(error.close)
                raise error
            response = (
                {"output": {"task_id": "test-task"}}
                if request.data
                else {
                    "output": {
                        "task_status": "SUCCEEDED",
                        "results": [{"url": "https://example.invalid/image.png"}],
                    }
                }
            )
            return io.BytesIO(json.dumps(response).encode("utf-8"))

        with (
            patch.object(GENERATOR.urllib.request, "urlopen", side_effect=urlopen),
            patch.object(GENERATOR.time, "sleep"),
        ):
            result = GENERATOR._compat(
                "sample", "model", "1024x1024", "test-key", "https://example.invalid/v1"
            )
        self.assertEqual(result, "https://example.invalid/image.png")
        self.assertEqual(requests[1]["parameters"]["size"], "1024*1024")


if __name__ == "__main__":
    unittest.main()
