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
        self.assertEqual(entries, [{"key": key, "original": None}])
        self.assertTrue(warnings)

    def test_decoded_marker_boundaries_and_nested_content(self):
        key = "0123456789abcdef01234567"
        self.backend[key] = SimpleNamespace(original_content="fixture original")
        for marker in (f"<<ccr:{key}>>", f"Retrieve original: hash={key}", f"Retrieve more: hash={key}"):
            for suffix in ("", "\nnext line", "\tmore"):
                with self.subTest(marker=marker, suffix=suffix):
                    messages = [{"content": [{"type": "text", "text": marker + suffix}], "metadata": object()}]
                    entries, warnings = self.worker._ccr_evidence(messages)
                    self.assertEqual(entries, [{"key": key, "original": "fixture original"}])
                    self.assertEqual(warnings, [])
        self.backend.clear()
        entries, warnings = self.worker._ccr_evidence([{"content": f"Retrieve original: hash={key}"}])
        self.assertEqual(entries, [{"key": key, "original": None}])
        self.assertTrue(warnings)

    def test_markers_inside_json_text_payloads_are_recognized(self):
        key = "0123456789abcdef01234567"
        self.backend[key] = SimpleNamespace(original_content="fixture original")
        for content in ('{"note": "Retrieve original: hash=%s"}' % key, '{"note": "Retrieve more: hash=%s\\nnext"}' % key):
            with self.subTest(content=content):
                self.assertEqual(self.worker._ccr_evidence([{"role": "tool", "content": content}]),
                                 ([{"key": key, "original": "fixture original"}], []))

    def test_markers_cannot_be_assembled_across_string_boundaries(self):
        self.assertEqual(self.worker._ccr_entries([{"content": "Retrieve original: hash="},
                                                  {"content": "0123456789abcdef01234567"}]), [])

    def test_missing_original_retains_reference_inventory(self):
        first, missing = "0123456789abcdef01234567", "abcdef0123456789abcdef01"
        self.backend[first] = SimpleNamespace(original_content="fixture original")
        entries, warnings = self.worker._ccr_evidence([
            {"content": f"<<ccr:{first}>> <<ccr:{missing}>>"}
        ])
        self.assertEqual(entries, [{"key": first, "original": "fixture original"},
                                   {"key": missing, "original": None}])
        self.assertEqual(len(entries), 2)
        self.assertTrue(warnings)

    def test_private_backend_drift_is_not_a_compression_failure(self):
        sys.modules["headroom.cache.compression_store"].get_compression_store = lambda: SimpleNamespace()
        entries, warnings = self.worker._ccr_evidence([{"content": "<<ccr:0123456789abcdef01234567>>"}])
        self.assertEqual(entries, [{"key": "0123456789abcdef01234567", "original": None}])
        self.assertIn("CCR evidence unavailable: AttributeError", warnings)

    def test_read_failure_preserves_all_recognized_keys(self):
        first, failed, later = "0123456789abcdef01234567", "fedcba9876543210fedcba98", "abcdef0123456789abcdef01"
        class Backend:
            def get(self, key):
                if key == failed:
                    raise OSError("fixture")
                return SimpleNamespace(original_content="original-" + key)
        sys.modules["headroom.cache.compression_store"].get_compression_store = lambda: SimpleNamespace(_backend=Backend())
        entries, warnings = self.worker._ccr_evidence([{"content": " ".join(f"<<ccr:{key}>>" for key in (first, failed, later))}])
        self.assertEqual(entries, [{"key": first, "original": "original-" + first}, {"key": failed, "original": None}, {"key": later, "original": "original-" + later}])
        self.assertTrue(warnings)

    def test_store_initialization_failure_preserves_recognized_key(self):
        key = "0123456789abcdef01234567"
        sys.modules["headroom.cache.compression_store"].get_compression_store = lambda: (_ for _ in ()).throw(OSError("fixture"))
        entries, warnings = self.worker._ccr_evidence([{"content": f"Retrieve original: hash={key}"}])
        self.assertEqual(entries, [{"key": key, "original": None}])
        self.assertTrue(warnings)

    def test_marker_prose_does_not_invalidate_recovered_inventory(self):
        key = "0123456789abcdef01234567"
        self.backend[key] = SimpleNamespace(original_content="fixture original")
        entries, warnings = self.worker._ccr_evidence([{ "content": f"Retrieve original: hash={key}"},
                                                     {"content": "syntax is <<ccr:<hash>>"}])
        self.assertEqual(entries, [{"key": key, "original": "fixture original"}])
        self.assertEqual(warnings, [])

    def test_explicit_toin_path_creates_no_extra_directory(self):
        self.assertIsNone(self.worker._TOIN_DIRECTORY)


if __name__ == "__main__":
    unittest.main()
