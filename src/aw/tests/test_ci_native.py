"""Native inventory and nightly results fail closed without downloading frameworks."""

import copy
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path
import unittest

from test_ci_checks import AW, GateFixture
from check_native import load
from native_acceptance import Commands, verify_bundle, verify_case
from native_summary import summarize


class NativeTests(GateFixture):
    def setUp(self) -> None:
        super().setUp()
        self.manifest = load(AW)

    def test_new_agent_requires_native_accounting(self) -> None:
        (self.root / "tests").mkdir()
        matrix = json.loads((AW / "tests/regression-matrix.json").read_text())
        matrix["agents"]["new-agent"] = {}
        (self.root / "tests/regression-matrix.json").write_text(json.dumps(matrix))
        (self.root / "tests/native-matrix.json").write_text(json.dumps(self.manifest))
        with self.assertRaisesRegex(ValueError, "every registered Agent"):
            load(self.root)

    def test_missing_entrypoint_and_empty_suites_fail(self) -> None:
        from unittest.mock import patch

        for mode in ("missing", "empty", "manual", "unpinned", "deadline"):
            manifest = copy.deepcopy(self.manifest)
            row = manifest["agents"]["agentscope"]
            if mode == "missing":
                row["cases"][0]["argv"][1] = "tests/not-present.py"
            elif mode == "empty":
                row["cases"] = []
            elif mode == "manual":
                row.update(mode="manual", reason="skip", guide="README.md")
            elif mode == "unpinned":
                row["requirements"] = ["agentscope>=2"]
            else:
                row["cases"][0]["timeout_seconds"] = 0
            original = Path.read_text

            def read(path: Path, *args: object, **kwargs: object) -> str:
                if path == AW / "tests/native-matrix.json":
                    return json.dumps(manifest)
                return original(path, *args, **kwargs)

            with self.subTest(mode=mode), patch.object(Path, "read_text", read):
                with self.assertRaises(ValueError):
                    load(AW)

    def reports(self) -> None:
        for agent, row in self.manifest["agents"].items():
            if row["mode"] == "manual":
                continue
            directory = self.root / agent
            directory.mkdir(exist_ok=True)
            (directory / "result.json").write_text(
                json.dumps(
                    {
                        "agent": agent,
                        "candidate_sha": "a" * 40,
                        "status": "passed",
                        "cases": [
                            {"name": case["name"], "status": "passed"} for case in row["cases"]
                        ],
                    }
                )
            )

    def test_missing_stale_skipped_or_failed_evidence_rejects_nightly(self) -> None:
        outcomes = {name: "success" for name in ("plan", "packages", "native")}
        self.reports()
        text, passed = summarize(self.manifest, self.root, "a" * 40, outcomes)
        self.assertTrue(passed)
        self.assertIn("MANUAL — not tested", text)
        for mode in ("missing", "stale", "empty", "failed-case", "failed-job", "skipped-job"):
            self.reports()
            jobs = dict(outcomes)
            path = self.root / "agentscope/result.json"
            report = json.loads(path.read_text())
            if mode == "missing":
                path.unlink()
            elif mode == "stale":
                report["candidate_sha"] = "b" * 40
            elif mode == "empty":
                report["cases"] = []
            elif mode == "failed-case":
                report["cases"][0]["status"] = "failed"
            else:
                jobs["packages"] = "failure" if mode == "failed-job" else "skipped"
            if mode != "missing":
                path.write_text(json.dumps(report))
            with self.subTest(mode=mode):
                self.assertFalse(summarize(self.manifest, self.root, "a" * 40, jobs)[1])

    def test_timeout_allows_fixture_to_reap_its_separate_process_group(self) -> None:
        child_pid = self.root / "child.pid"
        code = (
            "import os,signal,subprocess,sys,time\n"
            "from pathlib import Path\n"
            "child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'],start_new_session=True)\n"
            f"Path({str(child_pid)!r}).write_text(str(child.pid))\n"
            "try: time.sleep(60)\n"
            "finally:\n"
            " os.killpg(child.pid,signal.SIGKILL)\n"
            " child.wait()\n"
        )
        with self.assertRaises(subprocess.TimeoutExpired):
            Commands(self.root, dict(os.environ)).run([sys.executable, "-c", code], self.root, 1)
        with self.assertRaises(ProcessLookupError):
            os.kill(int(child_pid.read_text()), 0)

    def test_empty_reduced_and_skipped_native_suites_fail(self) -> None:
        case = {"name": "unit", "tests": 2, "report": "unittest"}
        verify_case(case, "Ran 2 tests in 0.1s\n\nOK\n")
        for output in (
            "Ran 0 tests in 0s\nOK\n",
            "Ran 1 test in 0s\nOK\n",
            "Ran 2 tests in 0s\nOK (skipped=1)\n",
            "",
        ):
            with self.subTest(output=output), self.assertRaises(ValueError):
                verify_case(case, output)
        case["report"] = "tap"
        output = "# tests 2\n# pass 2\n# fail 0\n# cancelled 0\n# skipped 0\n# todo 0\n"
        verify_case(case, output)
        with self.assertRaises(ValueError):
            verify_case(case, output.replace("# skipped 0", "# skipped 1"))

    def test_binary_bundle_must_match_source_and_checksums(self) -> None:
        (self.root / "source-sha.txt").write_text("a" * 40)
        names = (
            "aw",
            "aw-provider-tokenless",
            "aw-provider-sec-core",
            "tokenless",
            "rtk",
            "policy",
        )
        for name in names:
            file = self.root / name
            file.write_bytes(b"fixture")
            file.chmod(0o700)
        digest = hashlib.sha256(b"fixture").hexdigest()
        sums = self.root / "SHA256SUMS"
        sums.write_text("".join(f"{digest}  {name}\n" for name in names))
        verify_bundle(self.root, "a" * 40)
        with self.assertRaisesRegex(ValueError, "different commit"):
            verify_bundle(self.root, "b" * 40)
        (self.root / "aw").write_bytes(b"wrong")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            verify_bundle(self.root, "a" * 40)


if __name__ == "__main__":
    unittest.main()
