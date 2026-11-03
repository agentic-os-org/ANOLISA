"""Protocol tests for the resident L3 headroom pipeline worker.

Spawns ``benchmark/l3-scenario/assets/worker/headroom_pipeline_worker.py``
as a subprocess with a minimal stub ``headroom`` package on PYTHONPATH (the
worker normally runs inside the headroom venv, which this test environment
does not have) and drives the documented line-delimited JSON protocol.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

WORKER = (
    Path(__file__).resolve().parents[1]
    / "benchmark"
    / "l3-scenario"
    / "assets"
    / "worker"
    / "headroom_pipeline_worker.py"
)

_CONFIG_STUB = """\
class CacheAlignerConfig:
    def __init__(self, **kwargs):
        self.__dict__.update(kwargs)


class SmartCrusherConfig:
    def __init__(self, **kwargs):
        self.__dict__.update(kwargs)
"""

_STAGE_STUB = """\
class _Stage:
    def __init__(self, config=None):
        self.config = config


class {name}(_Stage):
    pass
"""

_PIPELINE_STUB = """\
from types import SimpleNamespace


class TransformPipeline:
    def __init__(self, transforms, provider):
        self._transforms = transforms
        self._provider = provider

    def apply(self, messages, model, model_limit=None):
        result = SimpleNamespace(
            messages=messages,
            tokens_before=len(messages),
            tokens_after=len(messages),
            transforms_applied=[type(t).__name__ for t in self._transforms],
            timing={},
            warnings=[],
        )
        if model.startswith("invalid-"):
            setattr(result, model.removeprefix("invalid-"), None)
        return result
"""

_ROUTER_STUB = """\
class ContentRouter:
    def __init__(self, config=None, observer=None):
        pass
"""


class WorkerProtocolTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="tokenless-worker-")
        self.addCleanup(self.temp.cleanup)
        stub_root = Path(self.temp.name)
        pkg = stub_root / "headroom"
        transforms = pkg / "transforms"
        transforms.mkdir(parents=True)
        (pkg / "__init__.py").touch()
        (transforms / "__init__.py").touch()
        (pkg / "config.py").write_text(_CONFIG_STUB, encoding="utf-8")
        (transforms / "cache_aligner.py").write_text(
            _STAGE_STUB.format(name="CacheAligner"), encoding="utf-8"
        )
        (transforms / "smart_crusher.py").write_text(
            _STAGE_STUB.format(name="SmartCrusher"), encoding="utf-8"
        )
        (transforms / "content_router.py").write_text(_ROUTER_STUB, encoding="utf-8")
        (transforms / "pipeline.py").write_text(_PIPELINE_STUB, encoding="utf-8")
        self.env = dict(os.environ, PYTHONPATH=str(stub_root), PYTHONDONTWRITEBYTECODE="1")

    def _run_lines(self, lines: list[str]) -> list[dict[str, Any]]:
        result = subprocess.run(
            [sys.executable, str(WORKER)],
            input="".join(line + "\n" for line in lines),
            capture_output=True,
            text=True,
            env=self.env,
            timeout=10,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        responses = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual(len(responses), len(lines) + 1, result.stdout)
        self.assertIs(responses[0]["ready"], True)
        return responses

    def _exchange(self, request: dict[str, Any]) -> dict[str, Any]:
        responses = self._run_lines([json.dumps(request), json.dumps(self._valid_request())])
        self.assertIs(responses[2]["ok"], True)
        self.assertEqual(responses[2]["variant"], "pure_stage")
        return responses[1]

    @staticmethod
    def _valid_request() -> dict[str, Any]:
        return {
            "messages": [{"role": "user", "content": "fixture"}],
            "model": "benchmark-model",
            "model_limit": 200_000,
            "variant": "pure_stage",
        }

    def test_handshake_and_clean_eof(self) -> None:
        responses = self._run_lines([])
        self.assertIn("pure_stage", responses[0]["variants"])
        self.assertIn("router", responses[0]["variants"])

    def test_unparseable_request_and_followup(self) -> None:
        responses = self._run_lines(["{not json", json.dumps(self._valid_request())])
        self.assertIs(responses[1]["ok"], False)
        self.assertIn("bad request json", responses[1]["error"])
        self.assertIs(responses[2]["ok"], True)

    def test_deeply_nested_request_and_followup(self) -> None:
        # Build the wire text directly so the test's encoder cannot hit its own limit.
        nested = "[" * 2000 + "0" + "]" * 2000
        request = '{"messages": [], "extra": ' + nested + "}"
        responses = self._run_lines([request, json.dumps(self._valid_request())])
        self.assertIs(responses[1]["ok"], False)
        self.assertIn("RecursionError", responses[1]["error"])
        self.assertIs(responses[2]["ok"], True)

    def test_invalid_result_metadata_and_followup(self) -> None:
        for field in ["transforms_applied", "timing", "warnings"]:
            with self.subTest(field=field):
                request = self._valid_request()
                request["model"] = f"invalid-{field}"
                response = self._exchange(request)
                self.assertIs(response["ok"], False)
                self.assertIn("TypeError:", response["error"])

    def test_non_object_request_and_followup(self) -> None:
        for request in ["just a string", [1, 2, 3], None, True, 42]:
            with self.subTest(request=request):
                responses = self._run_lines(
                    [json.dumps(request), json.dumps(self._valid_request())]
                )
                self.assertIs(responses[1]["ok"], False)
                self.assertIn("not a JSON object", responses[1]["error"])
                self.assertIs(responses[2]["ok"], True)

    def test_invalid_variant_and_followup(self) -> None:
        for variant in [["pure_stage"], {"name": "pure_stage"}, True, 42, 3.5]:
            with self.subTest(variant=variant):
                request = self._valid_request()
                request["variant"] = variant
                response = self._exchange(request)
                self.assertIs(response["ok"], False)
                self.assertIn("must be a string", response["error"])

    def test_valid_variants(self) -> None:
        for variant in ["pure_stage", "router"]:
            with self.subTest(variant=variant):
                request = self._valid_request()
                request["variant"] = variant
                response = self._exchange(request)
                self.assertIs(response["ok"], True)
                self.assertEqual(response["variant"], variant)

    def test_unknown_variant_and_followup(self) -> None:
        request = self._valid_request()
        request["variant"] = "missing"
        response = self._exchange(request)
        self.assertIs(response["ok"], False)
        self.assertIn("unknown variant", response["error"])

    def test_missing_variant_keeps_default(self) -> None:
        request = self._valid_request()
        del request["variant"]
        response = self._exchange(request)
        self.assertIs(response["ok"], True)
        self.assertEqual(response["variant"], "pure_stage")

    def test_falsy_variant_keeps_default(self) -> None:
        for variant in [None, "", False, 0, [], {}]:
            with self.subTest(variant=variant):
                request = self._valid_request()
                request["variant"] = variant
                response = self._exchange(request)
                self.assertIs(response["ok"], True)
                self.assertEqual(response["variant"], "pure_stage")


if __name__ == "__main__":
    unittest.main()
