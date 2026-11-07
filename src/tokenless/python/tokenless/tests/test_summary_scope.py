"""Public Python query API backed by real compression and SQLite state."""
import json
import re
import sqlite3
import tempfile
import unittest
from pathlib import Path

from anolisa_tokenless import TokenlessRuntime, TokenlessStats


class SummaryScopeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="summary-scope-")
        self.addCleanup(self.temporary.cleanup)
        self.path = Path(self.temporary.name)
        self.runtime = TokenlessRuntime(self.path, stats_enabled=True)
        original = json.dumps({"items": [f"item-{i}-" + "content " * 50 for i in range(12)]})
        for _ in range(2):
            result = self.runtime.compress_response(original, truncate_arrays_at=2)
            self.assertTrue(result.applied)
        marker = re.search(r"tokenless retrieve ([0-9a-f]{24})", result.output)
        self.assertIsNotNone(marker)
        self.runtime.retrieve(marker.group(1))
        self.stats = TokenlessStats(self.path)

    def test_partial_window_omits_unattributable_costs(self) -> None:
        partial = self.stats.summary(limit=1)
        self.assertIsNone(partial.attribution.net_savings_tokens)
        self.assertIsNone(partial.attribution.retrieved_tokens)
        self.assertIsNone(partial.attribution.retrieve_hits)
        self.assertEqual(partial.summary_scope.records_selected, 1)
        self.assertEqual(partial.summary_scope.records_total, 2)
        self.assertFalse(partial.summary_scope.complete)

    def test_complete_window_preserves_real_retrieval_attribution(self) -> None:
        complete = self.stats.summary(limit=2)
        self.assertEqual(complete.attribution.retrieve_hits, 1)
        self.assertGreater(complete.attribution.retrieved_tokens, 0)
        self.assertEqual(complete.attribution.net_savings_tokens,
                         complete.total.tokens_saved - complete.attribution.retrieved_tokens)
        self.assertTrue(complete.summary_scope.complete)

    def test_retrieve_query_failure_remains_best_effort(self) -> None:
        with sqlite3.connect(self.path / "stats.db") as connection:
            connection.execute("DROP TABLE retrieve_events")
        summary = self.stats.summary()
        self.assertEqual(summary.total.records, 2)
        self.assertIsNone(summary.attribution.net_savings_tokens)
        self.assertTrue(summary.summary_scope.complete)


if __name__ == "__main__":
    unittest.main()
