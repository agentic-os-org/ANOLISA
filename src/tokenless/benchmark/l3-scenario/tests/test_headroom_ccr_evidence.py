# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.

import importlib.util
import os
from pathlib import Path
import sys
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import patch

WORKER = Path(__file__).parents[1] / "assets/worker/headroom_pipeline_worker.py"


def load_worker():
    spec = importlib.util.spec_from_file_location("l3_worker_ccr", WORKER)
    module = importlib.util.module_from_spec(spec)
    stdout = sys.stdout
    try:
        with patch.dict(os.environ, {"HEADROOM_TOIN_PATH": "/fixture/toin.json"}):
            spec.loader.exec_module(module)
    finally:
        sys.stdout = stdout
    return module


class CcrEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.worker = load_worker()
        self.backend = {}
        module = ModuleType("headroom.cache.compression_store")
        module.get_compression_store = lambda: SimpleNamespace(_backend=self.backend)
        self.patch = patch.dict(sys.modules, {"headroom.cache.compression_store": module})
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def test_ordinary_hashes_never_become_missing_recovery_evidence(self):
        messages = [{"role": "user", "content": "hash=0123456789abcdef0123456789abcdef01234567"}]
        self.assertEqual(self.worker._ccr_entries(messages), [])
        self.assertEqual(self.worker._ccr_evidence(messages), ([], []))

    def test_long_digests_are_not_truncated_to_ccr_keys(self):
        messages = [{"content": "<<ccr:0123456789abcdef0123456789abcdef01234567>>"}]
        self.assertEqual(self.worker._ccr_entries(messages), [])

    def test_real_markers_retrieve_only_verified_originals(self):
        key = "0123456789abcdef01234567"
        self.backend[key] = SimpleNamespace(original_content="fixture original")
        for content in (f"<<ccr:{key}>>", f"Retrieve more: hash={key}.", f"[Retrieve original: hash={key}]"):
            with self.subTest(content=content):
                entries, warnings = self.worker._ccr_evidence([{"content": content}])
                self.assertEqual(entries, [{"key": key, "original": "fixture original"}])
                self.assertEqual(warnings, [])
        self.backend.clear()
        entries, warnings = self.worker._ccr_evidence([{"content": f"<<ccr:{key}>>"}])
        self.assertEqual(entries, [])
        self.assertTrue(warnings)

    def test_private_backend_drift_is_not_a_compression_failure(self):
        sys.modules["headroom.cache.compression_store"].get_compression_store = lambda: SimpleNamespace()
        entries, warnings = self.worker._ccr_evidence([{"content": "<<ccr:0123456789abcdef01234567>>"}])
        self.assertEqual(entries, [])
        self.assertEqual(warnings, ["CCR evidence unavailable: AttributeError"])

    def test_explicit_toin_path_creates_no_extra_directory(self):
        self.assertIsNone(self.worker._TOIN_DIRECTORY)


if __name__ == "__main__":
    unittest.main()
