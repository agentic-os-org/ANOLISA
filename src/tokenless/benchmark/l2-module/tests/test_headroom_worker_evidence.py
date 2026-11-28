# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.

import importlib.util
import os
from pathlib import Path
import sys
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import patch

WORKER = Path(__file__).parents[1] / "assets/worker/headroom_worker.py"


def load_worker():
    spec = importlib.util.spec_from_file_location("l2_worker_evidence", WORKER)
    module = importlib.util.module_from_spec(spec)
    stdout = sys.stdout
    try:
        with patch.dict(os.environ, {"HEADROOM_TOIN_PATH": "/fixture/toin.json"}):
            spec.loader.exec_module(module)
    finally:
        sys.stdout = stdout
    return module


class WorkerEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.worker = load_worker()
        cache = ModuleType("headroom.cache.compression_store")
        self.backend = {}
        cache.get_compression_store = lambda: SimpleNamespace(_backend=self.backend)
        self.modules = patch.dict(sys.modules, {"headroom.cache.compression_store": cache})
        self.modules.start()
        self.addCleanup(self.modules.stop)

    def test_ordinary_hashes_and_digest_prefixes_are_not_ccr(self):
        for text in (
            "hash=0123456789abcdef0123456789abcdef01234567",
            "Retrieve more: hash=0123456789abcdef0123456789abcdef01234567.",
            "<<ccr:0123456789abcdef0123456789abcdef01234567>>",
        ):
            with self.subTest(text=text):
                self.assertEqual(self.worker._ccr_entries(text), [])

    def test_actual_marker_forms_read_exact_stored_originals(self):
        key = "0123456789abcdef01234567"
        self.backend[key] = SimpleNamespace(original_content="original fixture")
        for text in (
            f"Retrieve more: hash={key}.",
            f"[config folded. Retrieve original: hash={key}]",
            f"<<ccr:{key},string,2.3KB>>",
            f"<<ccr:{key} 10_rows_offloaded>>",
        ):
            with self.subTest(text=text):
                self.assertEqual(self.worker._ccr_entries(text), [{"key": key, "original": "original fixture"}])

    def test_marker_boundaries_inside_json_and_lines(self):
        key = "0123456789abcdef01234567"
        self.backend[key] = SimpleNamespace(original_content="original fixture")
        for marker in (f"<<ccr:{key}>>", f"Retrieve original: hash={key}", f"Retrieve more: hash={key}"):
            for suffix in ("", "\nnext", '"}', "\\nnext"):
                with self.subTest(marker=marker, suffix=suffix):
                    self.assertEqual(self.worker._ccr_evidence(marker + suffix),
                                     ([{"key": key, "original": "original fixture"}], []))

    def test_backend_drift_degrades_only_recovery_evidence(self):
        key = "0123456789abcdef01234567"
        module = sys.modules["headroom.cache.compression_store"]
        module.get_compression_store = lambda: SimpleNamespace()
        entries, warnings = self.worker._ccr_evidence(f"<<ccr:{key}>>")
        self.assertEqual(entries, [])
        self.assertEqual(warnings, ["CCR evidence unavailable: AttributeError"])

    def test_unrecognized_marker_is_a_visible_evidence_gap(self):
        entries, warnings = self.worker._ccr_evidence("<<ccr:new-marker-format>>")
        self.assertEqual(entries, [])
        self.assertTrue(warnings)
        self.assertEqual(self.worker._ccr_evidence("ordinary unchanged content"), ([], []))

    def test_caller_supplied_toin_path_creates_no_temporary_directory(self):
        self.assertIsNone(self.worker._TOIN_DIRECTORY)


if __name__ == "__main__":
    unittest.main()
