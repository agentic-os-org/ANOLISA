"""Malformed provider responses should produce a bounded image diagnostic."""

import contextlib
import importlib.util
import io
import json
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/others/image-gen/scripts/generate_image.py"
)


class Response:
    def __init__(self, value):
        self.body = value if isinstance(value, bytes) else json.dumps(value).encode()

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return None

    def read(self):
        return self.body


class ImageResponseShapeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("image_shapes", SCRIPT)
        cls.image = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.image)

    def native(self, creation, status=None):
        replies = [Response(creation)]
        if status is not None:
            replies.append(Response(status))
        with (
            mock.patch.object(self.image.urllib.request, "urlopen", side_effect=replies) as request,
            mock.patch.object(self.image.time, "sleep"),
        ):
            result = self.image._wanx("fixture", "wanx2.1-t2i-turbo", "1024*1024", "test-key")
        return result, request.call_count

    def compatible(self, value):
        with mock.patch.object(self.image.urllib.request, "urlopen", return_value=Response(value)):
            return self.image._compat(
                "fixture", "other-model", "1024x1024", "test-key", "https://example.invalid/v1"
            )

    def assert_diagnostic(self, action):
        errors = io.StringIO()
        with contextlib.redirect_stderr(errors), self.assertRaises(SystemExit) as error:
            action()
        self.assertEqual(error.exception.code, 1)
        self.assertIn("ERROR:", errors.getvalue())
        self.assertNotIn("test-key", errors.getvalue())

    def test_non_object_creation_responses(self):
        for value in (None, [], "wrong", 42):
            with self.subTest(value=value):
                self.assert_diagnostic(lambda: self.native(value))

    def test_creation_output_and_task_id_shapes(self):
        for value in (
            {"output": []},
            {"output": "wrong"},
            {"output": {"task_id": 42}},
            {"output": {"task_id": ["task"]}},
        ):
            with self.subTest(value=value):
                self.assert_diagnostic(lambda: self.native(value))

    def test_poll_root_output_and_status_shapes(self):
        for value in ([], {"output": None}, {"output": []}, {"output": {"task_status": 42}}):
            with self.subTest(value=value):
                self.assert_diagnostic(lambda: self.native({"output": {"task_id": "task"}}, value))

    def test_native_results_and_image_field_shapes(self):
        for results in ("wrong", {}, [None], ["wrong"], [{"url": 42}], [{"b64_image": ["wrong"]}]):
            with self.subTest(results=results):
                status = {"output": {"task_status": "SUCCEEDED", "results": results}}
                self.assert_diagnostic(lambda: self.native({"output": {"task_id": "task"}}, status))

    def test_compatible_root_data_and_image_field_shapes(self):
        for value in (
            [],
            None,
            {"data": "wrong"},
            {"data": {}},
            {"data": [None]},
            {"data": ["wrong"]},
            {"data": [{"url": 42}]},
            {"data": [{"b64_json": ["wrong"]}]},
        ):
            with self.subTest(value=value):
                self.assert_diagnostic(lambda: self.compatible(value))

    def test_invalid_json_and_utf8_fail_cleanly_on_both_protocols(self):
        for payload in (b"{broken", b"\xff"):
            with self.subTest(payload=payload):
                self.assert_diagnostic(lambda: self.native(payload))
                self.assert_diagnostic(lambda: self.compatible(payload))

    def test_valid_native_url_and_base64_sources(self):
        for result, expected in (
            ({"url": "https://example.invalid/image.png"}, "https://example.invalid/image.png"),
            ({"b64_image": "YWJj"}, "b64:YWJj"),
        ):
            with self.subTest(result=result):
                value, calls = self.native(
                    {"output": {"task_id": "task"}},
                    {"output": {"task_status": "SUCCEEDED", "results": [result]}},
                )
                self.assertEqual(value, expected)
                self.assertEqual(calls, 2)

    def test_valid_compatible_url_and_base64_sources(self):
        self.assertEqual(
            self.compatible({"data": [{"url": "https://example.invalid/image.png"}]}),
            "https://example.invalid/image.png",
        )
        self.assertEqual(self.compatible({"data": [{"b64_json": "YWJj"}]}), "b64:YWJj")


if __name__ == "__main__":
    unittest.main()
