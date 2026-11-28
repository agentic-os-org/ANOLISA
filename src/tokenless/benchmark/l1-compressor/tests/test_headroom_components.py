# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Offline worker contracts, using pure functions and stub imports only.

Fact vectors match the Rust checks in src/component_cases.rs so the paired
components cannot score identical payloads with different matching rules.
"""

import importlib.util
from pathlib import Path
import sys
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import patch

PATH = Path(__file__).parents[1] / "assets/worker/headroom_components.py"
SPEC = importlib.util.spec_from_file_location("headroom_components_contract", PATH)
worker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(worker)


class FactRules(unittest.TestCase):
    def test_normalized_retry_accepts_format_changes_only(self):
        case = {"facts": ["required_heading", "complete explanatory evidence"], "patterns": [r"E\d{4}"]}
        escaped = "# required\\_heading\ncomplete\n  explanatory evidence E0308"
        scores = [SimpleNamespace(returncode=0, stdout='{"passed":3,"total":3,"missing":[]}'),
                  SimpleNamespace(returncode=0, stdout='{"passed":0,"total":3,"missing":["heading","body","regex"]}')]
        with patch.object(worker, "FACT_CHECKER", Path("/fixture/component_cases")), patch.object(worker.subprocess, "run", side_effect=scores) as invoked:
            self.assertEqual(worker.check_facts(case, escaped)["passed"], 3)
            frame = worker.check_facts(case, "# required\\_section\nexplanatory")
        self.assertEqual(invoked.call_count, 2)
        self.assertEqual(invoked.call_args.args[0][1], "--check-facts-stdin")
        self.assertEqual(frame["passed"], 0)
        self.assertEqual(len(frame["missing"]), 3)

    def test_case_changes_are_losses(self):
        case = {"facts": ["Disk Full"]}
        self.assertEqual(worker.check_facts(case, "disk full on /var")["passed"], 0)

    def test_search_records_bind_shared_headers(self):
        case = {"facts": ["src/a.rs:3:fn main() {}", "src/a.rs:9:fn other() {}"], "search_records": True}
        shared = (
            "[Search results grouped by file; all received lines retained]\n"
            'File="src/a.rs"\n3:fn main() {}\n9:fn other() {}\n'
        )
        self.assertEqual(worker.check_facts(case, shared)["passed"], 2)
        self.assertEqual(worker.check_facts(case, 'File="src/a.rs"\n3:fn main() {}\n')["passed"], 1)

    def test_search_paths_are_not_format_normalized(self):
        case = {"facts": ["src/a  b.rs:1:required match"], "search_records": True}
        self.assertEqual(worker.check_facts(case, "src/a b.rs:1:required match")["passed"], 0)
        self.assertEqual(worker.check_facts(case, "src/a  b.rs:1:required   match")["passed"], 1)
        case["facts"] = [r"src/a\_b.rs:1:required match"]
        self.assertEqual(worker.check_facts(case, "src/a_b.rs:1:required match")["passed"], 0)

    def test_rust_only_pattern_does_not_abort_other_rows_without_checker(self):
        encoder = SimpleNamespace(encode_ordinary=lambda text: list(text))
        case = {"id": "unicode", "domain": "json", "mode": "recovery", "input": "hello", "patterns": [r"\p{L}+"]}
        with patch.object(worker, "FACT_CHECKER", None):
            measured = worker.measure({"case": case}, 10, encoder)
        self.assertEqual(measured["error_kind"], "fact-check-unavailable")
        self.assertEqual(measured["disposition"], "unavailable")
        self.assertIsNone(measured["facts"])

    def test_python_compatible_regex_still_requires_rust_checker(self):
        encoder = SimpleNamespace(encode_ordinary=lambda text: list(text))
        for pattern in (r"[a-z&&b]", r"\w"):
            case = {"id": "dialect", "domain": "json", "mode": "recovery", "input": "a", "patterns": [pattern]}
            with patch.object(worker, "FACT_CHECKER", None):
                row = worker.measure({"case": case}, 10, encoder)
            self.assertEqual(row["error_kind"], "fact-check-unavailable")
            self.assertIsNone(row["original_facts"])

    def test_directory_headers_prefix_bare_file_names(self):
        records = worker.normalized_search_records("src/\nmain.rs:4:  fn main()\n")
        self.assertEqual(records, "src/main.rs:4:fn main()\n")

    def test_rust_line_semantics(self):
        self.assertEqual(worker.rust_lines("a\r\nb\n"), ["a", "b"])
        self.assertEqual(worker.rust_lines("a\u2028b"), ["a\u2028b"])

    def test_unicode_whitespace_matches_rust_not_python_extensions(self):
        for separator in ("\u001c", "\u001d", "\u001e", "\u001f"):
            with self.subTest(separator=separator):
                self.assertEqual(worker.check_facts({"facts": ["a b"]}, "a" + separator + "b")["passed"], 0)
                case = {"facts": ["src/a.rs:1:a b"], "search_records": True}
                self.assertEqual(worker.check_facts(case, "src/a.rs:1:a" + separator + "b")["passed"], 0)
        self.assertEqual(worker.check_facts({"facts": ["a b"]}, "a\u2003b")["passed"], 1)

    def test_candidate_checker_failure_preserves_measured_output_and_other_rows(self):
        encoder = SimpleNamespace(encode_ordinary=lambda text: list(text))
        case = {"id": "transient", "domain": "json", "mode": "recovery", "input": "original text", "patterns": [r"\w+"]}
        component = worker.Component("fixture", lambda text: "original")
        for error in (OSError("temporary"), worker.subprocess.TimeoutExpired("checker", 60), ValueError("bad score")):
            with self.subTest(error=type(error).__name__), patch.object(worker, "build_component", return_value=component), patch.object(worker, "ccr_entries", return_value=[]), patch.object(worker, "check_facts", side_effect=[{"passed": 1, "total": 1}, error]):
                row = worker.measure({"case": case}, 10, encoder)
                self.assertEqual(row["output"], "original")
                self.assertEqual(row["disposition"], "compressed")
                self.assertIsNone(row["facts"])
                self.assertEqual(row["error_kind"], "fact-check-unavailable")
                self.assertTrue(row["warnings"])


class Dispositions(unittest.TestCase):
    def test_missing_components_are_no_entry_not_zero(self):
        self.assertEqual(worker.disposition(None, "x", 1, None, "no-entry"), "no-entry")
        self.assertEqual(worker.disposition(None, "x", 1, None, "rejected"), "candidate-rejected")
        self.assertEqual(worker.disposition(None, "x", 1, None, "unavailable"), "unavailable")
        self.assertEqual(worker.disposition("x", "x", 1, 1, None), "passthrough")
        self.assertEqual(worker.disposition("yy", "x", 1, 2, None), "expanded")
        self.assertEqual(worker.disposition("y", "xx", 2, 1, None), "compressed")

    def test_retention_forms(self):
        ccr = [{"key": "0123456789ab", "original": "full"}]
        self.assertEqual(worker.retention_form("x", "x", None, [], "json"), "inline")
        self.assertEqual(worker.retention_form("y", "x", None, ccr, "json"), "offloaded-with-marker")
        self.assertEqual(worker.retention_form("y", "x", "smart:lossless:table", [], "json"), "inline")
        self.assertEqual(worker.retention_form("y", "x", "top_n", [], "json"), "unrecoverable-deletion")
        dangling = [{"key": "0123456789ab", "original": None}]
        self.assertEqual(worker.retention_form("y", "x", None, dangling, "buildlog"), "unrecoverable-deletion")
        self.assertEqual(worker.retention_form("y", "x", None, ccr + dangling, "search"), "unrecoverable-deletion")
        self.assertEqual(worker.retention_form("y", "x", None, [{"original": ""}], "json"), "offloaded-with-marker")

    def test_unmapped_domains_have_no_component(self):
        for domain in ("unclassified", "stack_trace"):
            self.assertIsNone(worker.build_component(domain, "direct-default"))

    def test_modules_missing_from_the_imported_revision_are_no_entry(self):
        missing = {name: None for name in (
            "headroom", "headroom.transforms", "headroom.transforms.lossless_compaction",
            "headroom.transforms.tabular_ingest", "headroom.transforms.text_crusher",
            "headroom.proxy", "headroom.proxy.tool_schema_compaction", "headroom.proxy.handlers",
            "headroom.proxy.handlers.openai", "headroom.transforms.smart_crusher", "headroom.transforms.search_compressor", "headroom.transforms.log_compressor", "headroom.transforms.html_extractor", "headroom.transforms.diff_compressor", "headroom.transforms.code_compressor")}
        with patch.dict(sys.modules, missing):
            for domain in ("json", "search", "paths", "tabular", "buildlog", "html", "diff", "source_code", "plain_text", "schema"):
                self.assertIsNone(worker.build_component(domain, "direct-default"), domain)

    def test_missing_direct_import_modules_are_classified_as_no_entry(self):
        encoder = SimpleNamespace(encode_ordinary=lambda text: list(text))
        with patch.dict(sys.modules, {"headroom": None}):
            for domain in ("json", "search", "buildlog", "html", "diff", "source_code"):
                entry = {"case": {"id": "fixture", "domain": domain, "mode": "direct-default", "input": "required_fact", "facts": ["required_fact"]}}
                measured = worker.measure(entry, 10, encoder)
                self.assertEqual(measured["disposition"], "no-entry", domain)

    def test_environment_failure_is_not_a_rejected_candidate(self):
        encoder = SimpleNamespace(encode_ordinary=lambda text: list(text))
        entry = {"case": {"id": "fixture", "domain": "json", "mode": "direct-default", "input": "required_fact", "facts": ["required_fact"]}}
        with patch.object(worker, "build_component", return_value=None):
            measured = worker.measure(entry, 10, encoder)
        self.assertEqual(measured["disposition"], "no-entry")
        self.assertEqual(measured["error_kind"], "no-entry")
        self.assertIsNone(measured["tokens_after"])
        with patch.object(worker, "build_component", side_effect=ImportError("fixture unavailable")):
            measured = worker.measure(entry, 10, encoder)
        self.assertEqual(measured["disposition"], "unavailable")

    def test_store_drift_preserves_compression_but_marks_recovery_unavailable(self):
        encoder = SimpleNamespace(encode_ordinary=lambda text: list(text))
        entry = {"case": {"id": "fixture", "domain": "json", "mode": "direct-default", "input": "required_fact " + "padding " * 10, "facts": ["required_fact"]}}
        component = worker.Component("fixture", lambda text: "required_fact")
        with patch.object(worker, "build_component", return_value=component), patch.object(worker, "ccr_entries", side_effect=AttributeError()):
            measured = worker.measure(entry, 10, encoder)
        self.assertEqual(measured["disposition"], "compressed")
        self.assertEqual(measured["retention_form"], "recovery-evidence-unavailable")
        self.assertTrue(measured["warnings"])

    def test_ordinary_digest_is_not_a_recovery_marker(self):
        self.assertEqual(worker.ccr_entries("hash=0123456789abcdef0123456789abcdef01234567"), [])

    def test_marker_boundaries_inside_json_and_lines(self):
        key = "0123456789abcdef01234567"
        store = ModuleType("headroom.cache.compression_store")
        store.get_compression_store = lambda: SimpleNamespace(_backend={key: SimpleNamespace(original_content="full")})
        with patch.dict(sys.modules, {"headroom.cache.compression_store": store}):
            for marker in (f"<<ccr:{key}>>", f"Retrieve original: hash={key}", f"Retrieve more: hash={key}"):
                for suffix in ("", "\nnext", '"}', "\\nnext"):
                    with self.subTest(marker=marker, suffix=suffix):
                        self.assertEqual(worker.ccr_entries(marker + suffix), [{"key": key, "original": "full"}])


if __name__ == "__main__":
    unittest.main()
