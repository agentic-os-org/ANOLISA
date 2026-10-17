"""Native inventory and nightly results fail closed without downloading frameworks."""

import ast
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
from native_reports import validate_case
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
            for case in row["cases"]:
                self.case_evidence(directory, case)
            (directory / "result.json").write_text(
                json.dumps(
                    {
                        "agent": agent,
                        "candidate_sha": "a" * 40,
                        "status": "passed",
                        "cases": [
                            {
                                "name": case["name"],
                                "status": "passed",
                                "log": case["name"] + ".log",
                            }
                            for case in row["cases"]
                        ],
                    }
                )
            )

    def case_evidence(self, directory: Path, case: dict) -> None:
        """Build passing artifacts so mutations exercise the real aggregation boundary."""
        log = ""
        if case["report"] == "unittest":
            log = f"Ran {case['tests']} tests in 0.1s\n\nOK\n"
        elif case["report"] == "tap":
            log = f"# tests {case['tests']}\n# pass {case['tests']}\n"
            log += "# fail 0\n# cancelled 0\n# skipped 0\n# todo 0\n"
        for result in case.get("results", []):
            document = {}
            for field, value in result.get("values", {}).items():
                parent = document
                parts = field.split("/")
                for part in parts[:-1]:
                    parent = parent.setdefault(part, {})
                parent[parts[-1]] = value
            if "cases" in result:
                spec = result["cases"]
                rows = [{spec["key"]: name, **spec.get("values", {})} for name in spec["names"]]
                if spec["at"]:
                    document[spec["at"]] = rows
                else:
                    document = rows
            path = directory / result["file"]
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(json.dumps(document))
        (directory / (case["name"] + ".log")).write_text(log)

    def custom_cases(self) -> list[tuple[str, dict]]:
        return [
            (agent, case)
            for agent, row in self.manifest["agents"].items()
            for case in row.get("cases", [])
            if case["report"] == "json"
        ]

    def test_every_native_case_requires_a_report_contract(self) -> None:
        for agent, row in self.manifest["agents"].items():
            for original in row.get("cases", []):
                for field in ("report", "tests" if original["report"] != "json" else "results"):
                    case = copy.deepcopy(original)
                    del case[field]
                    with self.subTest(agent=agent, case=case["name"], field=field):
                        with self.assertRaises(ValueError):
                            validate_case(case)
        case = copy.deepcopy(self.manifest["agents"]["pi"]["cases"][0])
        for names in ([], ["allow", "allow"]):
            case["results"][0]["cases"]["names"] = names
            with self.assertRaises(ValueError):
                validate_case(case)

    def test_custom_suites_reject_missing_empty_or_malformed_results(self) -> None:
        for agent, case in self.custom_cases():
            for result in case["results"]:
                for data in (None, "[]", "{}", "null", "not json"):
                    self.case_evidence(self.root, case)
                    path = self.root / result["file"]
                    if data is None:
                        path.unlink()
                    else:
                        path.write_text(data)
                    with self.subTest(
                        agent=agent, case=case["name"], file=result["file"], data=data
                    ):
                        with self.assertRaises(ValueError):
                            verify_case(case, "", self.root)

    def test_custom_suites_reject_reduced_renamed_duplicate_or_failed_cases(self) -> None:
        for agent, case in self.custom_cases():
            for result in case["results"]:
                self.case_evidence(self.root, case)
                verify_case(case, "", self.root)
                path = self.root / result["file"]
                valid = json.loads(path.read_text())
                if "cases" in result:
                    spec = result["cases"]
                    for mode in ("reduced", "renamed", "duplicate", "failed"):
                        data = copy.deepcopy(valid)
                        rows = data[spec["at"]] if spec["at"] else data
                        if mode == "reduced":
                            rows.pop()
                        elif mode == "renamed":
                            rows[0][spec["key"]] = "unexpected-case"
                        elif mode == "duplicate":
                            rows[0] = rows[1]
                        elif spec.get("values"):
                            rows[0][next(iter(spec["values"]))] = False
                        else:
                            continue
                        path.write_text(json.dumps(data))
                        with self.subTest(agent=agent, case=case["name"], mode=mode):
                            with self.assertRaises(ValueError):
                                verify_case(case, "", self.root)
                else:
                    for field, value in result["values"].items():
                        if "/" in field or type(value) is not int:
                            continue
                        data = copy.deepcopy(valid)
                        data[field] = value - 1
                        path.write_text(json.dumps(data))
                        with self.subTest(agent=agent, case=case["name"], field=field):
                            with self.assertRaises(ValueError):
                                verify_case(case, "", self.root)

    def test_zero_exit_noops_do_not_certify_custom_suites(self) -> None:
        for agent, case in self.custom_cases():
            directory = self.root / (agent + "-" + case["name"])
            directory.mkdir()
            output = Commands(directory, dict(os.environ)).run(
                [sys.executable, "-c", "pass"], directory, 10
            )
            with self.subTest(agent=agent, case=case["name"]):
                with self.assertRaises(ValueError):
                    verify_case(case, output, directory)

    def test_real_pi_fixture_with_empty_cases_fails_acceptance(self) -> None:
        fixture = AW / "crates/aw-service/tests/fixtures/pi"
        source = (fixture / "public-smoke.py").read_text()
        assignment = next(
            node
            for node in ast.parse(source).body
            if isinstance(node, ast.Assign)
            and any(
                isinstance(target, ast.Name) and target.id == "cases" for target in node.targets
            )
        )
        lines = source.splitlines(keepends=True)
        lines[assignment.lineno - 1 : assignment.end_lineno] = ["cases = {}\n"]
        script = self.root / "public-smoke.py"
        script.write_text("".join(lines))
        (self.root / "application.mjs").write_text((fixture / "application.mjs").read_text())
        output = subprocess.check_output(
            [
                sys.executable,
                "-B",
                str(script),
                "--aw",
                sys.executable,
                "--root",
                str(self.root / "npm"),
            ],
            text=True,
            timeout=10,
        )
        self.assertEqual(json.loads(output), [])
        case = self.manifest["agents"]["pi"]["cases"][0]
        with self.assertRaisesRegex(ValueError, "case names/count"):
            verify_case(case, output, self.root)

    def test_summary_rechecks_results_instead_of_trusting_passed_status(self) -> None:
        outcomes = {name: "success" for name in ("plan", "packages", "native")}
        for agent, case in self.custom_cases():
            self.reports()
            path = self.root / agent / case["results"][0]["file"]
            path.write_text("[]")
            with self.subTest(agent=agent, case=case["name"]):
                self.assertFalse(summarize(self.manifest, self.root, "a" * 40, outcomes)[1])
        self.reports()
        for agent, case in self.custom_cases():
            for result in case["results"]:
                (self.root / agent / result["file"]).unlink()
        self.assertFalse(summarize(self.manifest, self.root, "a" * 40, outcomes)[1])
        self.reports()
        (self.root / "openclaw/hooks.log").write_text("")
        self.assertFalse(summarize(self.manifest, self.root, "a" * 40, outcomes)[1])

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

    def test_ledger_failures_bound_wait_and_reap_descendants(self) -> None:
        for stage in ("open", "write", "flush"):
            directory = self.root / stage
            directory.mkdir()
            with self.subTest(stage=stage):
                result = subprocess.run(
                    [
                        sys.executable,
                        "-B",
                        str(AW / "tests/fixtures/native_ledger.py"),
                        str(directory),
                        stage,
                    ],
                    capture_output=True,
                    text=True,
                    timeout=15,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertTrue(json.loads(result.stdout.splitlines()[-1])["descendant_reaped"])

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
