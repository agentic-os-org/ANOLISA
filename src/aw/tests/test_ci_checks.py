"""Exercise the AW gate's failure boundaries without compiling Rust."""

import importlib.util
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

AW = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("aw_check", AW / "scripts/check.py")
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)


class GateTests(unittest.TestCase):
    def setUp(self) -> None:
        (AW / "target").mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="ci-check-", dir=AW / "target")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def git(self, *args: str) -> str:
        return subprocess.check_output(
            ["git", *args], cwd=self.root, text=True, stderr=subprocess.PIPE, timeout=10
        ).strip()

    def commit(self, path: str, text: str) -> str:
        file = self.root / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(text, encoding="utf-8")
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        return self.git("rev-parse", "HEAD")

    def init_git(self) -> str:
        self.git("init", "-q", "--initial-branch=main")
        self.git("config", "user.name", "AW fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        return self.commit("README.md", "base")

    def test_scope_uses_full_pr_and_base_advancement(self) -> None:
        base = self.init_git()
        self.git("checkout", "-qb", "feature")
        self.commit("src/aw/schema with spaces.json", "{}")
        head = self.commit("other.txt", "unrelated last commit")
        event = {"pull_request": {"base": {"sha": base}, "head": {"sha": head}}}
        self.assertTrue(gate.scope("pull_request", event, head, self.root))

        self.git("checkout", "-qb", "unrelated", base)
        head = self.commit("other.txt", "unrelated")
        self.git("checkout", "main")
        advanced = self.commit("src/aw/Cargo.lock", "base advanced")
        self.git("merge", "--no-edit", "unrelated")
        candidate = self.git("rev-parse", "HEAD")
        event["pull_request"] = {"base": {"sha": advanced}, "head": {"sha": head}}
        self.assertTrue(gate.scope("pull_request", event, candidate, self.root))

    def test_scope_handles_removal_noop_and_event_errors(self) -> None:
        base = self.init_git()
        head = self.commit("other.txt", "no AW change")
        self.assertFalse(gate.scope("push", {"before": base, "after": head}, head, self.root))
        before = self.commit("src/aw/schema.json", "{}")
        self.git("mv", "src/aw/schema.json", "schema.json")
        self.git("commit", "-qm", "move out of AW")
        after = self.git("rev-parse", "HEAD")
        self.assertTrue(gate.scope("push", {"before": before, "after": after}, after, self.root))
        for event in ("workflow_dispatch", "merge_group"):
            self.assertTrue(gate.scope(event, {}, after, self.root))
        self.assertTrue(gate.scope("push", {"before": "0" * 40, "after": after}, after, self.root))
        forced = {"before": "1" * 40, "after": after, "forced": True}
        self.assertTrue(gate.scope("push", forced, after, self.root))
        with self.assertRaises(ValueError):
            gate.scope("push", forced, base, self.root)
        with self.assertRaises(subprocess.CalledProcessError):
            gate.scope("push", {"before": "1" * 40, "after": after}, after, self.root)
        with self.assertRaises(ValueError):
            gate.scope("push", {"before": before, "after": base}, after, self.root)
        for event in ("pull_request_target", "unknown"):
            with self.assertRaises(ValueError):
                gate.scope(event, {}, after, self.root)
        with patch.dict(os.environ, {"GITHUB_SHA": base}):
            with self.assertRaises(ValueError):
                gate.candidate(self.root)
        for invalid in ("HEAD", "--help", "a" * 39, "a" * 40 + "\n"):
            with self.assertRaises(ValueError):
                gate.sha(invalid)

    def test_inventory_rejects_empty_ignored_and_missing_targets(self) -> None:
        cargo = self.root / "cargo"
        cargo.write_text(
            f"#!{sys.executable}\n"
            "import os, sys\n"
            "mode = os.environ['FIXTURE_INVENTORY']\n"
            "if mode.startswith('contract-empty-'):\n"
            "    target = mode.removeprefix('contract-empty-')\n"
            "    mode = 'empty' if target in sys.argv else 'valid'\n"
            "if mode == 'missing': sys.exit(7)\n"
            "if mode == 'empty' or (mode == 'valid' and '--ignored' in sys.argv):\n"
            "    print('0 tests, 0 benchmarks')\n"
            "else: print('contract: test\\n\\n1 test, 0 benchmarks')\n",
            encoding="utf-8",
        )
        cargo.chmod(0o755)
        for mode in (
            "valid", "empty", "ignored", "missing",
            "contract-empty-canonical", "contract-empty-schemas",
            "contract-empty-contracts", "contract-empty-orchestration",
        ):
            with self.subTest(mode=mode), patch.dict(
                os.environ,
                {
                    "PATH": str(self.root),
                    "FIXTURE_INVENTORY": mode,
                },
            ), patch.object(gate, "AW", self.root):
                if mode == "valid":
                    gate.check_inventory()
                else:
                    with self.assertRaises((ValueError, subprocess.CalledProcessError)):
                        gate.check_inventory()
        for malformed in (
            "",
            "noise",
            "0 tests, 0 benchmarks\nnoise",
            "test: test\n2 tests, 0 benchmarks",
        ):
            with self.assertRaises(ValueError):
                gate.inventory(malformed)

    def test_commands_fail_and_stop_the_sequence(self) -> None:
        commands = self.root / "commands.jsonl"
        cargo = self.root / "cargo"
        cargo.write_text(
            f"#!{sys.executable}\nimport json, sys\n"
            f"with open({str(commands)!r}, 'a') as out: out.write(json.dumps(sys.argv[1:]) + '\\n')\n"
            "if sys.argv[1] == 'fmt': sys.exit(7)\n",
            encoding="utf-8",
        )
        cargo.chmod(0o755)
        with patch.dict(os.environ, {"PATH": str(self.root)}), patch.object(
            gate, "AW", self.root
        ), patch.object(gate, "candidate", return_value="a" * 40), patch.object(
            gate, "selftest"
        ), self.assertRaises(
            subprocess.CalledProcessError
        ) as caught:
            gate.check()
        self.assertEqual(caught.exception.returncode, 7)
        self.assertEqual(
            [json.loads(line)[0] for line in commands.read_text().splitlines()], ["fmt"]
        )
        with self.assertRaises(FileNotFoundError):
            gate.run([str(self.root / "missing-tool")], self.root)

    def test_selftest_rejects_missing_empty_and_skipped_discovery(self) -> None:
        tests = self.root / "tests"
        tests.mkdir()
        module = tests / "test_ci_checks.py"
        with patch.object(gate, "AW", self.root):
            with self.assertRaises(subprocess.CalledProcessError):
                gate.selftest()
            module.write_text("# Empty test module\n", encoding="utf-8")
            with self.assertRaises(subprocess.CalledProcessError):
                gate.selftest()
            module.write_text(
                "import unittest\n@unittest.skip('fixture')\nclass Fixture(unittest.TestCase):\n"
                "    def test_skipped(self): self.fail('must not run')\n",
                encoding="utf-8",
            )
            with self.assertRaises(subprocess.CalledProcessError):
                gate.selftest()
            module.write_text(
                "import unittest\nclass Fixture(unittest.TestCase):\n"
                "    def test_present(self): self.assertTrue(True)\n",
                encoding="utf-8",
            )
            gate.selftest()

    def test_timeout_stops_an_ignoring_descendant(self) -> None:
        pid_file = self.root / "child.pid"
        child = (
            "import os, signal, time; from pathlib import Path; "
            "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
            f"Path({str(pid_file)!r}).write_text(str(os.getpid())); time.sleep(60)"
        )
        parent = (
            "import signal, subprocess, sys, time; "
            "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
            f"subprocess.Popen([sys.executable, '-c', {child!r}]); time.sleep(60)"
        )
        with self.assertRaises(subprocess.TimeoutExpired):
            gate.run([sys.executable, "-c", parent], self.root, timeout=1)
        self.assertTrue(pid_file.exists(), "descendant did not start before timeout")
        pid = int(pid_file.read_text())
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            status = Path(f"/proc/{pid}/stat")
            if not status.exists() or status.read_text().split()[2] == "Z":
                break
            time.sleep(0.02)
        else:
            os.kill(pid, signal.SIGKILL)
            self.fail("owned descendant remained running after timeout")

    def test_canonical_vectors_fail_even_with_python_optimization(self) -> None:
        script = self.root / "check_canonical.py"
        shutil.copyfile(AW / "tests/check_canonical.py", script)
        fixtures = self.root / "fixtures"
        fixtures.mkdir()
        original = json.loads((AW / "tests/fixtures/canonical-vectors.json").read_text())
        damaged = json.loads(json.dumps(original))
        damaged[0]["digest"] = "0" * 64
        for vector, valid in ((original, True), ([], False), ({}, False), (damaged, False)):
            (fixtures / "canonical-vectors.json").write_text(json.dumps(vector), encoding="utf-8")
            for optimization in ([], ["-O"]):
                with self.subTest(valid=valid, optimization=optimization):
                    result = subprocess.run(
                        [sys.executable, *optimization, str(script)],
                        capture_output=True,
                        text=True,
                        timeout=20,
                    )
                    self.assertEqual(result.returncode == 0, valid, result.stderr)
                    if vector == damaged:
                        self.assertIn("Python canonical digest differs", result.stderr)

    def test_required_result_truth_table(self) -> None:
        sha = "a" * 40
        valid = {
            "GITHUB_SHA": sha,
            "SCOPE_RESULT": "success",
            "CHECKS_RESULT": "success",
            "SELECTED": "true",
            "CANDIDATE_SHA": sha,
            "TESTED_SHA": sha,
        }
        gate.required(valid)
        gate.required({**valid, "SELECTED": "false", "CHECKS_RESULT": "skipped", "TESTED_SHA": ""})
        failures = [{key: ""} for key in valid]
        failures += [{"CHECKS_RESULT": value} for value in ("failure", "cancelled", "skipped")]
        failures += [
            {"SCOPE_RESULT": "failure"},
            {"SELECTED": "false"},
            {"TESTED_SHA": "b" * 40},
            {"CANDIDATE_SHA": "b" * 40},
            {"SELECTED": "unknown"},
        ]
        for changes in failures:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                gate.required({**valid, **changes})


if __name__ == "__main__":
    unittest.main()
