"""Installed-wheel tests for the native Tokenless Python runtime."""

from __future__ import annotations

import json
import re
import sqlite3
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from importlib.metadata import distribution
from pathlib import Path
from unittest.mock import patch

from anolisa_tokenless import (
    StatsDiffSort,
    StatsMode,
    StatsNotFoundError,
    StatsOperation,
    TokenlessError,
    TokenlessRuntime,
    TokenlessStats,
    __version__,
)


class TokenlessRuntimeTests(unittest.TestCase):
    """Exercise public Python API behavior against real SQLite state."""

    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory(prefix="tokenless-python-test-")
        self.runtime = TokenlessRuntime(
            self.temporary_directory.name,
            stats_enabled=True,
        )

    def tearDown(self) -> None:
        del self.runtime
        self.temporary_directory.cleanup()

    @staticmethod
    def long_response(sentinel: str = "ORCHID-7291") -> str:
        return json.dumps(
            {
                "items": [f"{sentinel}-record-{index:04d}" for index in range(200)],
                "tail": f"RECOVERY_SENTINEL={sentinel}\n",
            },
            ensure_ascii=False,
        )

    def test_version_matches_component(self) -> None:
        self.assertRegex(__version__, r"^\d+\.\d+\.\d+$")

    def test_distribution_contains_license_and_documentation_link(self) -> None:
        package = distribution("anolisa-tokenless")
        self.assertEqual(package.metadata.get_all("License-File"), ["LICENSE"])

        license_paths = [
            path
            for path in package.files or ()
            if str(path).endswith(".dist-info/licenses/LICENSE")
        ]
        self.assertEqual(len(license_paths), 1)
        self.assertIn("Apache License", Path(license_paths[0].locate()).read_text())

        metadata = package.read_text("METADATA")
        self.assertIsNotNone(metadata)
        assert metadata is not None
        self.assertIn(
            "https://github.com/alibaba/anolisa/blob/main/src/tokenless/README.md",
            metadata,
        )
        self.assertNotIn("../../README.md", metadata)

    def test_compress_and_retrieve_byte_exact(self) -> None:
        payload = "RECOVERY_SENTINEL=ORCHID-7291\n" + ("世界" * 200)
        original = json.dumps({"tail": payload}, ensure_ascii=False)
        result = self.runtime.compress_response(
            original,
            truncate_strings_at=96,
            max_depth=8,
            agent_id="python-test",
            session_id="session-a",
            tool_use_id="tool-a",
        )
        self.assertTrue(result.applied)
        self.assertLess(len(result.output.encode()), len(original.encode()))
        marker = re.search(
            r"If needed, run in shell: tokenless retrieve ([0-9a-f]{24})", result.output
        )
        self.assertIsNotNone(marker)
        assert marker is not None
        recovered = self.runtime.retrieve(marker.group(1).upper())
        self.assertEqual(recovered, payload)

    def test_stats_query_empty_database_and_invalid_path(self) -> None:
        data_dir = Path(self.temporary_directory.name, "empty-stats")
        stats = TokenlessStats(data_dir)

        self.assertTrue(stats.status.available)
        self.assertEqual(stats.status.records, 0)
        self.assertEqual(stats.summary().total.records, 0)
        self.assertEqual(stats.list(), ())
        with patch.dict("os.environ", {"TOKENLESS_DATA_DIR": str(data_dir)}):
            self.assertEqual(TokenlessStats().status.data_dir, str(data_dir))
        with self.assertRaisesRegex(ValueError, "absolute path"):
            TokenlessStats("relative")

    def test_stats_query_full_read_only_surface(self) -> None:
        data_dir = Path(self.temporary_directory.name, "stats-query")
        original = self.long_response("STATS-SENTINEL")
        baseline = TokenlessRuntime(
            data_dir,
            compression_enabled=False,
            stats_enabled=True,
        )
        baseline_result = baseline.compress_response(
            original,
            truncate_arrays_at=2,
            agent_id="python-stats",
            session_id="baseline-session",
            tool_use_id="tool-baseline",
        )
        self.assertEqual(baseline_result.disposition, "dry_run")
        del baseline

        active = TokenlessRuntime(data_dir, stats_enabled=True)
        active_result = active.compress_response(
            original,
            truncate_arrays_at=2,
            agent_id="python-stats",
            session_id="tokenless-session",
            tool_use_id="tool-active",
        )
        self.assertTrue(active_result.applied)
        with sqlite3.connect(Path(data_dir, "stats.db")) as connection:
            connection.execute(
                "UPDATE stats SET applied_operations = ?, recoverability = ? "
                "WHERE session_id = ?",
                ('["json_cleanup","toon"]', "lossless", "tokenless-session"),
            )

        stats = TokenlessStats(data_dir)
        status = stats.status
        self.assertTrue(status.available)
        self.assertEqual(status.records, 2)
        self.assertEqual(Path(status.data_dir), data_dir)

        summary = stats.summary()
        # Summaries report active rows only; the dry-run baseline is excluded.
        self.assertEqual(summary.total.records, 1)
        self.assertEqual(summary.dry_run_records_excluded, 1)
        self.assertEqual(summary.attribution.gross_savings_tokens, summary.total.tokens_saved)
        self.assertEqual(summary.attribution.retrieve_hits, 0)
        self.assertEqual(summary.attribution.retrieved_tokens, 0)
        self.assertEqual(summary.attribution.net_savings_tokens, summary.total.tokens_saved)
        self.assertGreater(summary.total.tokens_saved, 0)
        self.assertIn(StatsOperation.COMPRESS_RESPONSE, summary.by_operation)
        self.assertEqual(stats.summary(limit=1).total.records, 1)

        records = stats.list()
        self.assertEqual(len(records), 2)
        self.assertEqual(records[0].session_id, "tokenless-session")
        self.assertEqual(records[0].mode, StatsMode.ACTIVE)
        self.assertEqual(records[0].applied_operations, ("json_cleanup", "toon"))
        self.assertEqual(records[0].recoverability, "lossless")
        self.assertIsNone(records[0].before_text)
        self.assertIsNone(records[0].after_text)
        self.assertEqual(len(stats.list(limit=1)), 1)

        shown = stats.show(records[0].id)
        self.assertEqual(shown.before_text, original)
        self.assertIsNotNone(shown.after_text)
        self.assertGreater(shown.tokens_saved, 0)
        with self.assertRaises(StatsNotFoundError):
            stats.show(9_999_999)

        record_diff = stats.diff(record_id=shown.id, context=1)
        self.assertEqual(record_diff.scope.kind, "record")
        self.assertEqual(record_diff.scope.record_id, shown.id)
        self.assertIsNotNone(record_diff.chains[0].diff)

        session_diff = stats.diff(
            session_id="tokenless-session",
            limit=1,
            sort=StatsDiffSort.TIME,
        )
        self.assertEqual(session_diff.scope.kind, "session")
        self.assertEqual(len(session_diff.chains), 1)
        self.assertIsNone(session_diff.chains[0].diff)

        tool_diff = stats.diff(
            session_id="tokenless-session",
            tool_use_id="tool-active",
            context=0,
        )
        self.assertEqual(tool_diff.scope.kind, "tool-use")
        self.assertEqual(tool_diff.scope.tool_use_id, "tool-active")
        self.assertIsNotNone(tool_diff.chains[0].diff)

        comparison = stats.compare("baseline-session", "tokenless-session")
        self.assertGreater(comparison.baseline_tokens, comparison.tokenless_tokens)
        self.assertGreater(comparison.saved_tokens, 0)
        self.assertIn(
            StatsOperation.COMPRESS_RESPONSE,
            comparison.baseline_by_operation,
        )

        with self.assertRaisesRegex(ValueError, "exactly one"):
            stats.diff()
        with self.assertRaisesRegex(ValueError, "exactly one"):
            stats.diff(record_id=shown.id, session_id="tokenless-session")
        with self.assertRaisesRegex(ValueError, "requires session_id"):
            stats.diff(record_id=shown.id, tool_use_id="tool-active")
        with self.assertRaisesRegex(ValueError, "positive integer"):
            stats.list(limit=0)
        with self.assertRaisesRegex(ValueError, "record_id"):
            stats.show(0)
        with self.assertRaisesRegex(ValueError, "non-negative"):
            stats.diff(record_id=shown.id, context=-1)
        with self.assertRaises(ValueError):
            stats.diff(record_id=shown.id, sort="invalid")
        with self.assertRaises(StatsNotFoundError):
            stats.diff(session_id="missing-session")
        with self.assertRaisesRegex(
            StatsNotFoundError,
            "baseline session 'missing-session'",
        ):
            stats.compare("missing-session", "tokenless-session")
        with self.assertRaisesRegex(
            StatsNotFoundError,
            "Tokenless session 'missing-session'",
        ):
            stats.compare("baseline-session", "missing-session")

    def test_stats_query_reports_unavailable_database(self) -> None:
        data_dir = Path(self.temporary_directory.name, "broken-stats-query")
        data_dir.mkdir()
        Path(data_dir, "stats.db").write_bytes(b"not a sqlite database")

        stats = TokenlessStats(data_dir)
        self.assertFalse(stats.status.available)
        self.assertIsNotNone(stats.status.error)
        with self.assertRaises(TokenlessError):
            stats.summary()

    def test_invalid_json_raises_package_error(self) -> None:
        with self.assertRaisesRegex(TokenlessError, "JSON parse error"):
            self.runtime.compress_response("not-json")

    def test_missing_hash_raises_package_error(self) -> None:
        with self.assertRaisesRegex(TokenlessError, "no stashed payload"):
            self.runtime.retrieve("000000000000000000000000")

    def test_malformed_hash_raises_package_error(self) -> None:
        with self.assertRaisesRegex(TokenlessError, "invalid stash hash"):
            self.runtime.retrieve("not-a-hash")

    def test_stash_initialization_failure_can_still_apply_lossless_json(self) -> None:
        with tempfile.TemporaryDirectory(prefix="tokenless-python-broken-stash-") as directory:
            Path(directory, "stash.db").write_bytes(b"not a sqlite database")
            runtime = TokenlessRuntime(directory, stats_enabled=False)
            self.assertFalse(runtime.stash_available)
            self.assertIsNotNone(runtime.stash_error)

            original = self.long_response()
            result = runtime.compress_response(
                original,
                truncate_arrays_at=2,
                agent_id="python-test",
            )
            self.assertEqual(result.disposition, "applied")
            self.assertEqual(json.loads(result.output), json.loads(original))
            self.assertLess(len(result.output), len(original))

    def test_short_string_limit_is_reversible_fail_open(self) -> None:
        original = json.dumps({"tail": "x" * 400})
        result = self.runtime.compress_response(original, truncate_strings_at=10)

        self.assertEqual(result.disposition, "recoverability_unavailable")
        self.assertEqual(result.output, original)
        self.assertEqual(result.stash_errors, 0)
        self.assertEqual(result.unrecoverable_truncations, 1)

    def test_string_stash_write_failure_is_reversible_fail_open(self) -> None:
        with tempfile.TemporaryDirectory(prefix="tokenless-python-string-failure-") as directory:
            runtime = TokenlessRuntime(directory, stats_enabled=False)
            with sqlite3.connect(Path(directory, "stash.db")) as connection:
                connection.execute("DROP TABLE stash")

            original = json.dumps({"tail": "x" * 400})
            result = runtime.compress_response(original, truncate_strings_at=120)
            self.assertEqual(result.disposition, "recoverability_unavailable")
            self.assertEqual(result.output, original)
            self.assertEqual(result.stash_errors, 1)
            self.assertEqual(result.unrecoverable_truncations, 1)

    def test_depth_stash_write_failure_is_reversible_fail_open(self) -> None:
        with tempfile.TemporaryDirectory(prefix="tokenless-python-depth-failure-") as directory:
            runtime = TokenlessRuntime(directory, stats_enabled=False)
            with sqlite3.connect(Path(directory, "stash.db")) as connection:
                connection.execute("DROP TABLE stash")

            original = json.dumps({"nested": {"payload": "x" * 400}})
            result = runtime.compress_response(original, max_depth=0)
            self.assertEqual(result.disposition, "recoverability_unavailable")
            self.assertEqual(result.output, original)
            self.assertEqual(result.stash_errors, 1)
            self.assertEqual(result.unrecoverable_truncations, 1)

    def test_parallel_calls_do_not_cross_attribution_or_state(self) -> None:
        def compress(index: int) -> str:
            result = self.runtime.compress_response(
                self.long_response(f"SENTINEL-{index}"),
                truncate_arrays_at=2,
                agent_id="python-test",
                session_id=f"session-{index}",
                tool_use_id=f"tool-{index}",
            )
            # Head+tail truncation keeps the head items (truncate_arrays_at=2)
            # and the default 8 tail items inline with the truncation marker
            # in between, so cross-call contamination surfaces at both ends.
            self.assertIn(f"SENTINEL-{index}-record-0000", result.output)
            self.assertIn(f"SENTINEL-{index}-record-0199", result.output)
            self.assertIn("190 items omitted", result.output)
            match = re.search(
                r"If needed, run in shell: tokenless retrieve ([0-9a-f]{24})", result.output
            )
            self.assertIsNotNone(match)
            assert match is not None
            return self.runtime.retrieve(match.group(1))

        with ThreadPoolExecutor(max_workers=8) as executor:
            recovered = list(executor.map(compress, range(16)))
        for index, payload in enumerate(recovered):
            # The stash holds only the dropped middle segment
            # (records 0002..0191); head and tail items stay inline.
            self.assertEqual(
                json.loads(payload),
                [f"SENTINEL-{index}-record-{record:04d}" for record in range(2, 192)],
            )

        with sqlite3.connect(f"{self.temporary_directory.name}/stats.db") as connection:
            rows = connection.execute(
                "SELECT session_id, tool_use_id FROM stats WHERE agent_id = 'python-test'"
            ).fetchall()
        self.assertEqual(len(rows), 16)
        self.assertEqual(
            set(rows),
            {(f"session-{index}", f"tool-{index}") for index in range(16)},
        )


if __name__ == "__main__":
    unittest.main()
