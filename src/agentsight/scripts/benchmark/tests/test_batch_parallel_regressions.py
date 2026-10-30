"""Bound real analyzer processes while preserving ordered batch output."""

import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from test_batch_analyze import batch_analyze


@unittest.skipUnless(os.name == "posix", "local executable fixtures use POSIX file locks")
class BatchParallelTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.sessions = self.root / "sessions"
        self.sessions.mkdir()
        for index in range(6):
            path = self.sessions / f"s{index}.jsonl"
            path.write_text(
                json.dumps({"type": "user", "message": {"content": "fixture " * 200}}) + "\n",
                encoding="utf-8",
            )
            os.utime(path, (200 - index, 200 - index))
        self.control = self.root / "control.json"
        self.modes = self.root / "modes.json"
        self.reset()
        self.analyzer = self.root / "analyzer.py"
        self.analyzer.write_text(
            f"#!{sys.executable}\n"
            "import fcntl,json,sys,time\nfrom pathlib import Path\n"
            f"control=Path({str(self.control)!r}); modes=Path({str(self.modes)!r})\n"
            "doc=json.loads(Path(sys.argv[1]).read_text(encoding='utf-8')); sid=doc['session_id']\n"
            "def record(start):\n"
            " with control.open('r+',encoding='utf-8') as stream:\n"
            "  fcntl.flock(stream,fcntl.LOCK_EX); state=json.load(stream)\n"
            "  state['active'] += 1 if start else -1\n"
            "  state['maximum']=max(state['maximum'],state['active'])\n"
            "  state['started' if start else 'finished'].append(sid)\n"
            "  stream.seek(0); json.dump(state,stream); stream.truncate()\n"
            "record(True)\n"
            "try:\n"
            " time.sleep(0.18 if sid=='s0' else 0.02)\n"
            " mode=json.loads(modes.read_text()).get(sid,'ok')\n"
            " if mode=='exit': sys.exit(1)\n"
            " print('[]' if mode=='array' else '{' if mode=='json' else json.dumps({'fixture':sid}))\n"
            "finally: record(False)\n",
            encoding="utf-8",
        )
        self.analyzer.chmod(0o700)

    def reset(self, modes: dict | None = None) -> None:
        self.control.write_text(
            json.dumps({"active": 0, "maximum": 0, "started": [], "finished": []}), encoding="utf-8"
        )
        self.modes.write_text(json.dumps(modes or {}), encoding="utf-8")

    def cli(self, *extra: str, binary: str | None = None) -> subprocess.CompletedProcess[str]:
        result = subprocess.run(
            [
                sys.executable,
                batch_analyze.__file__,
                "--dir",
                str(self.sessions),
                "--dim",
                "perf",
                "--min-steps",
                "1",
                "--binary",
                binary or str(self.analyzer),
                *extra,
            ],
            env={
                **os.environ,
                "TMPDIR": str(self.root),
                "TEMP": str(self.root),
                "TMP": str(self.root),
            },
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=20,
        )
        self.assertFalse(list(self.root.glob("atif-*.json")))
        return result

    def documents(self, text: str) -> list[dict]:
        decoder, documents = json.JSONDecoder(), []
        while text.strip():
            document, end = decoder.raw_decode(text.lstrip())
            documents.append(document)
            text = text.lstrip()[end:]
        return documents

    def state(self) -> dict:
        return json.loads(self.control.read_text(encoding="utf-8"))

    def test_default_and_explicit_single_job_have_identical_results(self) -> None:
        default = self.cli()
        self.assertEqual(default.returncode, 0, default.stderr)
        self.assertEqual(self.state()["maximum"], 1)
        self.reset()
        explicit = self.cli("--jobs", "1")
        self.assertEqual(explicit.returncode, 0, explicit.stderr)
        self.assertEqual(default.stdout, explicit.stdout)
        self.assertEqual(default.stderr, explicit.stderr)
        self.assertEqual(self.state()["maximum"], 1)

    def test_out_of_order_processes_emit_in_discovery_order_with_a_cap(self) -> None:
        result = self.cli("--jobs", "3")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            [item["fixture"] for item in self.documents(result.stdout)], [f"s{i}" for i in range(6)]
        )
        state = self.state()
        self.assertGreater(state["maximum"], 1)
        self.assertLessEqual(state["maximum"], 3)
        self.assertEqual(state["active"], 0)
        self.assertNotEqual(state["finished"][0], "s0")
        positions = [
            result.stderr.index(f"File: {self.sessions / f's{i}.jsonl'}") for i in range(6)
        ]
        self.assertEqual(positions, sorted(positions))
        self.assertIn("Done: 6 analyzed, 0 skipped, 0 failed", result.stderr)

    def test_limit_prevents_extra_processes(self) -> None:
        result = self.cli("--jobs", "3", "--limit", "2")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.state()["started"]), 2)
        self.assertEqual(len(self.documents(result.stdout)), 2)

    def test_mixed_failures_do_not_hide_later_successes(self) -> None:
        self.reset({"s1": "exit", "s2": "array", "s3": "json"})
        result = self.cli("--jobs", "3")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            [item["fixture"] for item in self.documents(result.stdout)], ["s0", "s4", "s5"]
        )
        self.assertIn("Done: 3 analyzed, 0 skipped, 3 failed", result.stderr)
        self.assertEqual(self.state()["active"], 0)

    def test_missing_binary_is_counted_per_file(self) -> None:
        result = self.cli("--jobs", "3", binary=str(self.root / "missing"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertIn("Done: 0 analyzed, 0 skipped, 6 failed", result.stderr)

    def test_empty_and_short_files_do_not_spawn_analyzers(self) -> None:
        (self.sessions / "s1.jsonl").write_text("{}\n" * 400, encoding="utf-8")
        result = self.cli("--jobs", "3", "--min-steps", "2")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.state()["started"], [])
        self.assertIn("Done: 0 analyzed, 6 skipped, 0 failed", result.stderr)

    def test_invalid_worker_counts_are_rejected(self) -> None:
        for value in ("0", "-1", "9", "1.5"):
            with self.subTest(value=value):
                result = self.cli("--jobs", value)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.state()["started"], [])

    def test_zero_limit_does_not_start_a_process(self) -> None:
        result = self.cli("--jobs", "3", "--limit", "0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.state()["started"], [])

    def test_timeout_isolated_and_temp_files_removed(self) -> None:
        arguments = [
            "batch-analyze.py",
            "--dir",
            str(self.sessions),
            "--dim",
            "perf",
            "--min-steps",
            "1",
            "--jobs",
            "3",
            "--binary",
            "fixture",
        ]
        paths = []

        def invoke(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
            paths.append(Path(command[1]))
            raise subprocess.TimeoutExpired("fixture", 300)

        errors = io.StringIO()
        with patch.object(sys, "argv", arguments), patch.object(
            batch_analyze.subprocess, "run", side_effect=invoke
        ):
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(errors):
                batch_analyze.main()
        self.assertEqual(len(paths), 6)
        self.assertTrue(all(not path.exists() for path in paths))
        self.assertIn("Done: 0 analyzed, 0 skipped, 6 failed", errors.getvalue())

    def test_a_blocked_first_file_keeps_outstanding_work_bounded(self) -> None:
        release, started = threading.Event(), threading.Event()
        calls, lock = [], threading.Lock()

        def analyze(path: Path, *args: object) -> tuple[str, dict, str]:
            with lock:
                calls.append(path)
                if len(calls) == 3:
                    started.set()
            if path.name == "s0.jsonl":
                self.assertTrue(release.wait(5))
            return "analyzed", {}, ""

        files = [self.sessions / f"s{i}.jsonl" for i in range(6)]
        with patch.object(batch_analyze, "analyze_file", side_effect=analyze):
            results = batch_analyze.analyze_ordered(files, 3, 1, ["fixture"], "perf")
            collector = threading.Thread(target=lambda: list(results))
            collector.start()
            try:
                self.assertTrue(started.wait(5))
                self.assertEqual(len(calls), 3)
            finally:
                release.set()
                collector.join(5)
            self.assertFalse(collector.is_alive())
        self.assertEqual(len(calls), 6)


if __name__ == "__main__":
    unittest.main()
