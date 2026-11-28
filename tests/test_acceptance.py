"""Acceptance must reject missing, skipped, stale and contradictory evidence."""

import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import acceptance


class AcceptanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.manifest = self.root / "manifest.json"
        self.data = {"schema_version": 1, "candidate_sha": "a" * 40, "reports": []}

    def report(self, name: str, xml: str, required: bool = True) -> None:
        payload = xml.encode()
        (self.root / f"{name}.xml").write_bytes(payload)
        self.data["reports"].append(
            {
                "id": name,
                "component": "tokenless",
                "kind": "regression",
                "required": required,
                "exit_code": 0,
                "path": f"{name}.xml",
                "sha256": hashlib.sha256(payload).hexdigest(),
            }
        )

    def evaluate(self) -> dict:
        self.manifest.write_text(json.dumps(self.data))
        return acceptance.evaluate(self.manifest)

    def test_required_pass_and_optional_skip(self) -> None:
        self.report("required", '<testsuite tests="1"><testcase name="works"/></testsuite>')
        self.report(
            "optional", '<testsuite><testcase name="x"><skipped/></testcase></testsuite>', False
        )
        result = self.evaluate()
        self.assertEqual(result["status"], "passed")
        self.assertEqual(result["reports"][1]["status"], "skipped")
        self.assertEqual(result["kind"], "component_regression")

    def test_required_nonpassing_evidence_fails(self) -> None:
        for xml, status in [
            ('<testsuite><testcase name="x"><failure/></testcase></testsuite>', "failed"),
            ('<testsuite><testcase name="x"><error/></testcase></testsuite>', "failed"),
            ('<testsuite><testcase name="x"><skipped/></testcase></testsuite>', "skipped"),
            ('<testsuite tests="0"/>', "blocked"),
            ('<testsuite tests="2"><testcase name="x"/></testsuite>', "blocked"),
            ('<testsuite errors="1"><testcase name="x"/></testsuite>', "blocked"),
            ('<testsuite><error/><testcase name="x"/></testsuite>', "blocked"),
            ('<testsuite><testcase name="x" status="notrun"/></testsuite>', "blocked"),
            (
                '<testsuite><testcase name="x"><skipped/><failure/></testcase></testsuite>',
                "failed",
            ),
            ('<!DOCTYPE x><testsuite><testcase name="x"/></testsuite>', "blocked"),
            ("not XML", "blocked"),
        ]:
            with self.subTest(xml=xml):
                self.data["reports"] = []
                self.report("required", xml)
                result = self.evaluate()
                self.assertEqual(result["status"], "failed")
                self.assertEqual(result["reports"][0]["status"], status)

    def test_missing_tampered_and_escaped_reports_fail(self) -> None:
        self.report("required", '<testsuite><testcase name="x"/></testsuite>')
        path = self.root / "required.xml"
        path.write_text("changed")
        self.assertEqual(self.evaluate()["status"], "failed")
        path.unlink()
        self.assertEqual(self.evaluate()["status"], "failed")
        self.data["reports"][0]["path"] = "../outside.xml"
        self.assertEqual(self.evaluate()["status"], "failed")

    def test_process_failure_overrides_successful_xml(self) -> None:
        self.report("required", '<testsuite><testcase name="x"/></testsuite>')
        self.data["reports"][0]["exit_code"] = 42
        self.assertEqual(self.evaluate()["status"], "failed")
        self.report("optional", '<testsuite><testcase name="x"/></testsuite>', False)
        self.data["reports"][0]["exit_code"] = 0
        self.data["reports"][1]["exit_code"] = 42
        (self.root / "optional.xml").unlink()
        self.assertEqual(self.evaluate()["status"], "failed")

    def run_cli(self) -> subprocess.CompletedProcess:
        self.manifest.write_text(json.dumps(self.data))
        return subprocess.run(
            [sys.executable, acceptance.__file__, str(self.manifest)],
            capture_output=True,
            text=True,
            check=False,
            timeout=5,
        )

    def test_optional_failure_survives_inconsistent_counts(self) -> None:
        for outcome in ("failure", "error"):
            with self.subTest(outcome=outcome):
                self.data["reports"] = []
                self.report("required", '<testsuite><testcase name="ok"/></testsuite>')
                self.report(
                    "optional",
                    f'<testsuite tests="2"><testcase name="bad"><{outcome}/></testcase></testsuite>',
                    False,
                )
                process = self.run_cli()
                self.assertEqual(process.returncode, 1)
                result = json.loads(process.stdout)
                self.assertEqual(result["status"], "failed")
                self.assertEqual(result["reports"][1]["status"], "failed")

    def test_utf16_dtds_and_entities_are_rejected(self) -> None:
        for encoding in ("utf-16", "utf-16-le", "utf-16-be"):
            for declaration, content in [
                ("<!DOCTYPE testsuite>", ""),
                (
                    '<!DOCTYPE testsuite [<!ENTITY text "expanded payload">]>',
                    "<system-out>&text;</system-out>",
                ),
            ]:
                with self.subTest(encoding=encoding, declaration=declaration):
                    self.data["reports"] = []
                    self.report("required", '<testsuite><testcase name="ok"/></testsuite>')
                    payload = (
                        declaration + '<testsuite><testcase name="ok"/>' + content + "</testsuite>"
                    ).encode(encoding)
                    (self.root / "required.xml").write_bytes(payload)
                    self.data["reports"][0]["sha256"] = hashlib.sha256(payload).hexdigest()
                    process = self.run_cli()
                    self.assertEqual(process.returncode, 1)
                    self.assertEqual(json.loads(process.stdout)["reports"][0]["status"], "blocked")

    def test_invalid_manifest_and_cli_fail_closed(self) -> None:
        self.report("required", '<testsuite><testcase name="x"/></testsuite>')
        for field, value in [("required", "true"), ("kind", "native"), ("component", "invented")]:
            with self.subTest(field=field):
                before = self.data["reports"][0][field]
                self.data["reports"][0][field] = value
                with self.assertRaises(ValueError):
                    self.evaluate()
                self.data["reports"][0][field] = before
        self.data["reports"][0]["required"] = False
        with self.assertRaises(ValueError):
            self.evaluate()
        process = subprocess.run(
            [sys.executable, acceptance.__file__, str(self.manifest)],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(process.returncode, 1)
        self.assertEqual(json.loads(process.stdout)["status"], "blocked")


if __name__ == "__main__":
    unittest.main()
