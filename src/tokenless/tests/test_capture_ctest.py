#!/usr/bin/env python3
"""Tests for pinned source validation in capture_ctest.py."""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

TESTS_DIR = Path(__file__).resolve().parent
COMPONENT_ROOT = TESTS_DIR.parent
if str(TESTS_DIR) not in sys.path:
    sys.path.insert(0, str(TESTS_DIR))

import capture_ctest  # noqa: E402


def git(repo: Path, *args: str) -> str:
    return subprocess.check_output(["git", *args], cwd=repo, stderr=subprocess.STDOUT).decode()


def init_repo(repo: Path, filename: str, content: str) -> str:
    repo.mkdir(parents=True, exist_ok=True)
    subprocess.check_call(["git", "init", "--quiet", str(repo)])
    git(repo, "config", "user.name", "Tokenless Test")
    git(repo, "config", "user.email", "tokenless-test@example.com")
    (repo / filename).write_text(content, encoding="utf-8")
    git(repo, "add", filename)
    git(repo, "commit", "--quiet", "-m", f"add {filename}")
    return git(repo, "rev-parse", "HEAD").strip()


class _FakeSelectorKey:
    def __init__(self, fileobj: object, data: object) -> None:
        self.fileobj = fileobj
        self.data = data


class _FakeSelector:
    """POSIX-independent selector with level-triggered readiness."""

    def __init__(self) -> None:
        self._entries: dict[object, object] = {}

    def __enter__(self) -> "_FakeSelector":
        return self

    def __exit__(self, *_exc: object) -> None:
        return None

    def register(self, fileobj: object, _events: int, data: object) -> None:
        self._entries[fileobj] = data

    def unregister(self, fileobj: object) -> None:
        del self._entries[fileobj]

    def get_map(self) -> dict[object, object]:
        return self._entries

    def select(self, timeout: float = 1.0) -> list[tuple[_FakeSelectorKey, int]]:
        # Level-triggered like the real selector: a registered pipe stays
        # reported until capture() unregisters it at EOF.
        return [
            (_FakeSelectorKey(fileobj, data), 1)
            for fileobj, data in list(self._entries.items())
        ]


class TestCaptureCtestSourcePinning(unittest.TestCase):
    def test_rejects_unrelated_cached_repo(self) -> None:
        _, tag, expected_commit, _, _ = capture_ctest.PROJECTS["brotli"]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            init_repo(root / "sources" / "brotli", "README.md", "unrelated\n")
            with self.assertRaises(SystemExit) as ctx:
                capture_ctest.resolve_source(
                    "brotli",
                    "google/brotli",
                    tag,
                    expected_commit,
                    root / "dest",
                    root / "sources",
                )
            self.assertIn(expected_commit, str(ctx.exception))

    def test_rejects_missing_cached_project_without_network(self) -> None:
        repo, tag, expected_commit, _, _ = capture_ctest.PROJECTS["brotli"]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "sources").mkdir()
            with self.assertRaises(SystemExit) as ctx:
                capture_ctest.resolve_source(
                    "brotli",
                    repo,
                    tag,
                    expected_commit,
                    root / "dest",
                    root / "sources",
                )
            self.assertIn("missing cached repository", str(ctx.exception))

    def test_accepts_exact_head_and_ancestor_commit_from_cache(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cached = root / "sources" / "demo"
            pinned_commit = init_repo(cached, "src.txt", "pinned\n")
            git(cached, "tag", "-a", "-m", "v1.0.0", "v1.0.0")

            dest_exact = root / "dest_exact"
            resolved_exact, clone_exact = capture_ctest.resolve_source(
                "demo",
                "example/demo",
                "v1.0.0",
                pinned_commit,
                dest_exact,
                root / "sources",
            )
            self.assertEqual(resolved_exact, pinned_commit)
            self.assertEqual(
                clone_exact,
                "git clone --depth 1 file://<cached> (pinned tag+commit, unmodified)",
            )
            self.assertEqual((dest_exact / "src.txt").read_text(encoding="utf-8"), "pinned\n")

            (cached / "src.txt").write_text("newer\n", encoding="utf-8")
            git(cached, "add", "src.txt")
            git(cached, "commit", "--quiet", "-m", "advance head")

            dest_ancestor = root / "dest_ancestor"
            resolved_ancestor, clone_ancestor = capture_ctest.resolve_source(
                "demo",
                "example/demo",
                "v1.0.0",
                pinned_commit,
                dest_ancestor,
                root / "sources",
            )
            self.assertEqual(resolved_ancestor, pinned_commit)
            self.assertIn("detached checkout", clone_ancestor)
            self.assertEqual(
                (dest_ancestor / "src.txt").read_text(encoding="utf-8"), "pinned\n"
            )

    def test_rejects_missing_or_mismatched_cached_tag(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cached = root / "sources" / "demo"
            pinned_commit = init_repo(cached, "src.txt", "pinned\n")

            with self.assertRaises(SystemExit) as missing_ctx:
                capture_ctest.resolve_source(
                    "demo",
                    "example/demo",
                    "v1.0.0",
                    pinned_commit,
                    root / "dest_missing",
                    root / "sources",
                )
            self.assertIn("missing tag v1.0.0", str(missing_ctx.exception))

            (cached / "src.txt").write_text("newer\n", encoding="utf-8")
            git(cached, "add", "src.txt")
            git(cached, "commit", "--quiet", "-m", "advance head")
            git(cached, "tag", "v1.0.0")

            with self.assertRaises(SystemExit) as mismatch_ctx:
                capture_ctest.resolve_source(
                    "demo",
                    "example/demo",
                    "v1.0.0",
                    pinned_commit,
                    root / "dest_mismatch",
                    root / "sources",
                )
            self.assertIn("cached tag v1.0.0 resolves to", str(mismatch_ctx.exception))

    def test_accepts_relative_sources_dir_for_shallow_and_detached_clones(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cached = root / "cache" / "demo"
            pinned_commit = init_repo(cached, "src.txt", "pinned\n")
            git(cached, "tag", "-a", "-m", "v1.0.0", "v1.0.0")

            old_cwd = os.getcwd()
            os.chdir(root)
            try:
                resolved_exact, clone_exact = capture_ctest.resolve_source(
                    "demo",
                    "example/demo",
                    "v1.0.0",
                    pinned_commit,
                    Path("out_exact"),
                    Path("cache"),
                )
                self.assertEqual(resolved_exact, pinned_commit)
                self.assertEqual(
                    clone_exact,
                    "git clone --depth 1 file://<cached> (pinned tag+commit, unmodified)",
                )
                self.assertEqual((root / "out_exact" / "src.txt").read_text(encoding="utf-8"), "pinned\n")

                (cached / "src.txt").write_text("newer\n", encoding="utf-8")
                git(cached, "add", "src.txt")
                git(cached, "commit", "--quiet", "-m", "advance head")

                resolved_detached, clone_detached = capture_ctest.resolve_source(
                    "demo",
                    "example/demo",
                    "v1.0.0",
                    pinned_commit,
                    Path("out_detached"),
                    Path("cache"),
                )
                self.assertEqual(resolved_detached, pinned_commit)
                self.assertIn("detached checkout", clone_detached)
                self.assertEqual(
                    (root / "out_detached" / "src.txt").read_text(encoding="utf-8"), "pinned\n"
                )
            finally:
                os.chdir(old_cwd)

    def test_projects_match_committed_provenance(self) -> None:
        for rel_path, expected_count in (
            ("crates/tokenless-compressors/tests/fixtures/build_logs/ctest/provenance.json", 6),
            ("crates/tokenless-compressors/tests/fixtures/build_logs/cmake_build/provenance.json", 8),
        ):
            entries = json.loads((COMPONENT_ROOT / rel_path).read_text(encoding="utf-8"))
            self.assertEqual(len(entries), expected_count)
            by_repo = {
                entry["environment"]["repository"]: (
                    entry["environment"]["tag"],
                    entry["environment"]["commit"],
                    entry["environment"]["cmake_options"],
                )
                for entry in entries
            }
            for repo, (tag, commit, cmake_opts) in by_repo.items():
                matching = [
                    spec for spec in capture_ctest.PROJECTS.values() if spec[0] == repo
                ]
                self.assertEqual(len(matching), 1)
                self.assertEqual((tag, commit, cmake_opts), matching[0][1:4])

        ctest_entries = json.loads(
            (
                COMPONENT_ROOT
                / "crates/tokenless-compressors/tests/fixtures/build_logs/ctest/provenance.json"
            ).read_text(encoding="utf-8")
        )
        ctest_repos = {entry["environment"]["repository"] for entry in ctest_entries}
        for repo, *_ in capture_ctest.PROJECTS.values():
            self.assertIn(repo, ctest_repos)

        cc_entries = json.loads(
            (
                COMPONENT_ROOT
                / "crates/tokenless-compressors/tests/fixtures/build_logs/cc_compile/provenance.json"
            ).read_text(encoding="utf-8")
        )
        self.assertEqual(len(cc_entries), 8)
        all_specs = {
            **{name: spec[:4] for name, spec in capture_ctest.PROJECTS.items()},
            **{
                name: (repo, tag, commit, [])
                for name, (repo, tag, commit) in capture_ctest.NATIVE_MAKE_PROJECTS.items()
            },
        }
        cc_repos = set()
        for entry in cc_entries:
            env = entry["environment"]
            cc_repos.add(env["repository"])
            matching = [spec for spec in all_specs.values() if spec[0] == env["repository"]]
            self.assertEqual(len(matching), 1)
            _, expected_tag, expected_commit, base_opts = matching[0]
            self.assertEqual((env["tag"], env["commit"]), (expected_tag, expected_commit))
            for opt in base_opts:
                self.assertIn(opt, env["cmake_options"])
        self.assertIn("facebook/zstd", cc_repos)

    def test_suite_flag_defaults_to_ctest_and_accepts_build_and_verbose_suites(self) -> None:
        parser = capture_ctest.build_parser()
        default_args = parser.parse_args(["--project", "brotli", "--output", "/tmp/out"])
        self.assertEqual(default_args.suite, "ctest")
        for suite in ("ctest", "build", "verbose", "all"):
            parsed = parser.parse_args(
                ["--project", "brotli", "--suite", suite, "--output", "/tmp/out"]
            )
            self.assertEqual(parsed.suite, suite)
        zstd_parsed = parser.parse_args(
            ["--project", "zstd", "--suite", "verbose", "--output", "/tmp/out"]
        )
        self.assertEqual((zstd_parsed.project, zstd_parsed.suite), ("zstd", "verbose"))

    def test_verbose_suite_runs_match_cc_compile_corpus_sweep(self) -> None:
        cc_compile_test = (
            COMPONENT_ROOT / "crates/tokenless-compressors/tests/cc_compile.rs"
        ).read_text(encoding="utf-8")
        entries_block = re.search(
            r"let entries:\s*&\[\(&str,\s*&str,\s*bool\)\]\s*=\s*&\[(.*?)\];",
            cc_compile_test,
            re.DOTALL,
        )
        self.assertIsNotNone(entries_block)
        assert entries_block is not None
        sweep_runs = {
            (project, run, expect_compacted == "true")
            for project, run, expect_compacted in re.findall(
                r'\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*,\s*(true|false)\s*,?\s*\)',
                entries_block.group(1),
            )
        }
        producer_runs = {
            (project, name, expect_zero)
            for project in ("brotli", "catch2", "fmt", "zstd")
            for name, _, _, expect_zero in capture_ctest.verbose_runs_for_project(project)
        }
        self.assertEqual(len(sweep_runs), 16)
        self.assertEqual(sweep_runs, producer_runs)

    def test_failure_diagnostic_markers_cover_failure_runs(self) -> None:
        self.assertIn("clang_ninja_verbose_failure", capture_ctest.FAILURE_DIAGNOSTIC_MARKERS)
        self.assertIn("failure_unknown_target", capture_ctest.FAILURE_DIAGNOSTIC_MARKERS)
        self.assertIn(
            "FAILED:",
            capture_ctest.FAILURE_DIAGNOSTIC_MARKERS["clang_ninja_verbose_failure"],
        )

    def test_build_run_provenance_records_per_run_setup(self) -> None:
        failure_run = next(
            entry
            for entry in capture_ctest.verbose_runs_for_project("brotli")
            if entry[0] == "clang_ninja_verbose_failure"
        )
        _, per_run_prep, cmd_args, _ = failure_run
        record = {
            "exit_code": 1,
            "duration_seconds": 0.5,
            "sha256": {"observed": "deadbeef"},
        }
        prov = capture_ctest.build_run_provenance(per_run_prep, cmd_args, record)
        self.assertEqual(prov["setup"], per_run_prep)
        flattened = [arg for step in per_run_prep for arg in step]
        self.assertIn("-DCMAKE_C_COMPILER=clang", flattened)
        self.assertIn("-DCMAKE_C_FLAGS=-Weverything -Werror", flattened)
        self.assertEqual(prov["options"], cmd_args)
        self.assertEqual(prov["exit_code"], 1)

    def test_verbose_suite_commands_match_committed_provenance(self) -> None:
        cc_entries = json.loads(
            (
                COMPONENT_ROOT
                / "crates/tokenless-compressors/tests/fixtures/build_logs/cc_compile/provenance.json"
            ).read_text(encoding="utf-8")
        )
        by_project_and_run = {
            (project, name): cmd_args
            for project in ("brotli", "catch2", "fmt", "zstd")
            for name, _, cmd_args, _ in capture_ctest.verbose_runs_for_project(project)
        }
        for entry in cc_entries:
            filename = entry["file"]
            self.assertTrue(filename.endswith(".txt"), filename)
            project, run_name = filename[: -len(".txt")].split("_", 1)
            expected_cmd = by_project_and_run[(project, run_name)]
            recorded_cmd = entry["capture"]["command"]
            image_id = entry["environment"]["image_id"]
            self.assertIn(image_id, recorded_cmd)
            img_idx = recorded_cmd.index(image_id)
            self.assertEqual(recorded_cmd[img_idx + 1 :], expected_cmd)


class TestCaptureCtestDeadline(unittest.TestCase):
    def test_final_wait_is_bounded_after_both_pipes_close(self) -> None:
        # A child that closes both output pipes before exiting used to leave the
        # selector loop while capture() then waited forever at proc.wait().
        timeout_seconds = 2
        script = "import os, time; os.close(1); os.close(2); time.sleep(30)"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with (
                mock.patch.object(
                    capture_ctest.selectors, "DefaultSelector", _FakeSelector
                ),
                mock.patch.object(capture_ctest.subprocess, "run") as cleanup,
            ):
                started = time.monotonic()
                with self.assertRaises(TimeoutError) as ctx:
                    capture_ctest.capture(
                        [sys.executable, "-c", script],
                        ["python", "-c", "<close pipes and sleep>"],
                        root / "capture",
                        container_name="tokenless-deadline-test",
                        timeout_seconds=timeout_seconds,
                    )
                elapsed = time.monotonic() - started

            self.assertIn(f"exceeded {timeout_seconds}s", str(ctx.exception))
            # Far below the child's 30s sleep; the 8s allowance covers startup.
            self.assertLess(elapsed, timeout_seconds + 8)
            cleanup.assert_called_once()
            self.assertEqual(
                cleanup.call_args.args[0],
                ["docker", "rm", "-f", "tokenless-deadline-test"],
            )
            self.assertFalse((root / "capture" / "capture.json").exists())

    def test_zero_remaining_after_pipes_close_expires(self) -> None:
        # Drive the clock forward after both pipes reach EOF while the child is
        # still alive, to cover the explicit remaining <= 0 guard.
        clock = mock.Mock()
        clock.time.side_effect = [0.0, 0.0, 5.0]
        script = "import os, time; os.close(1); os.close(2); time.sleep(30)"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with (
                mock.patch.object(capture_ctest, "time", clock),
                mock.patch.object(
                    capture_ctest.selectors, "DefaultSelector", _FakeSelector
                ),
                mock.patch.object(capture_ctest.subprocess, "run") as cleanup,
            ):
                with self.assertRaises(TimeoutError) as ctx:
                    capture_ctest.capture(
                        [sys.executable, "-c", script],
                        ["python", "-c", "<close pipes and sleep>"],
                        root / "capture",
                        container_name="tokenless-zero-budget-test",
                        timeout_seconds=2,
                    )
            self.assertIn("exceeded 2s", str(ctx.exception))
            cleanup.assert_called_once()
            self.assertFalse((root / "capture" / "capture.json").exists())

    def test_completed_capture_still_writes_a_record(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with mock.patch.object(
                capture_ctest.selectors, "DefaultSelector", _FakeSelector
            ):
                record = capture_ctest.capture(
                    [sys.executable, "-c", "pass"],
                    ["python", "-c", "pass"],
                    root / "capture",
                    container_name="tokenless-completed-test",
                    timeout_seconds=10,
                )
            capture_dir = root / "capture"
            self.assertEqual(record["exit_code"], 0)
            self.assertEqual(record["observed_bytes"], 0)
            self.assertEqual((capture_dir / "stdout.txt").read_bytes(), b"")
            self.assertEqual((capture_dir / "stderr.txt").read_bytes(), b"")
            self.assertTrue((capture_dir / "capture.json").exists())

    def test_child_output_is_recorded_with_level_triggered_selector(self) -> None:
        # A real selector keeps reporting a registered pipe until EOF. With an
        # edge-triggered fake, read1() consuming data without unregistering left
        # capture() spinning until the deadline instead of recording the output.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with mock.patch.object(
                capture_ctest.selectors, "DefaultSelector", _FakeSelector
            ):
                record = capture_ctest.capture(
                    [sys.executable, "-c", "print('hello world', end='')"],
                    ["python", "-c", "print('hello world', end='')"],
                    root / "capture",
                    container_name="tokenless-output-test",
                    timeout_seconds=10,
                )
            self.assertEqual(record["exit_code"], 0)
            self.assertEqual(record["observed_bytes"], 11)
            stdout = (root / "capture" / "stdout.txt").read_bytes()
            self.assertEqual(stdout, b"hello world")
            self.assertTrue((root / "capture" / "capture.json").exists())


if __name__ == "__main__":
    unittest.main()
