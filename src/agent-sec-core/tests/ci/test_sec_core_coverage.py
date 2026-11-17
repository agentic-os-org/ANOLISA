"""Cross-runner report integrity checks for the sec-core coverage gate."""

import shutil
import subprocess
import sys
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path
from tempfile import TemporaryDirectory

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts/ci"))

from sec_core_coverage import normalize_report, validate_report  # noqa: E402

DIFF_COVER = shutil.which("diff-cover") or str(
    Path(sys.executable).with_name("diff-cover")
)


class CoverageReports(unittest.TestCase):
    def setUp(self):
        self.temp = TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.component = Path(self.temp.name) / "src/agent-sec-core"
        self.source = self.component / "v2/crates/example/src/lib.rs"
        self.source.parent.mkdir(parents=True)
        self.source.write_text("pub fn example() {}\n")
        self.report = self.component / "coverage.xml"

    def write_report(self, source, filename, lines=1):
        root = ET.Element("coverage", {"lines-valid": str(lines)})
        ET.SubElement(ET.SubElement(root, "sources"), "source").text = str(source)
        item = ET.SubElement(root, "class", {"filename": str(filename)})
        ET.SubElement(
            ET.SubElement(item, "lines"), "line", {"number": "1", "hits": "2"}
        )
        ET.ElementTree(root).write(self.report)

    def test_report_remains_valid_after_moving_to_another_runner(self):
        self.write_report(self.component / "v2", "crates/example/src/lib.rs")
        normalize_report(self.report, self.component)
        moved = Path(self.temp.name) / "other-runner/component"
        moved.parent.mkdir()
        self.component.rename(moved)
        report = moved / "coverage.xml"
        validate_report(report, moved)
        self.assertEqual(ET.parse(report).find(".//line").attrib["hits"], "2")
        self.assertEqual(
            ET.parse(report).find(".//class").attrib["filename"],
            "v2/crates/example/src/lib.rs",
        )

    def test_absolute_source_is_normalized(self):
        self.write_report(self.component, self.source)
        normalize_report(self.report, self.component)
        validate_report(self.report, self.component)

    def test_missing_report_fails(self):
        with self.assertRaises(FileNotFoundError):
            validate_report(self.report, self.component)

    def test_empty_report_fails(self):
        self.write_report(self.component, self.source, lines=0)
        with self.assertRaises(ValueError):
            normalize_report(self.report, self.component)

    def test_unknown_or_external_source_fails(self):
        outside = Path(self.temp.name) / "external.rs"
        outside.write_text("fn external() {}")
        for source in (outside, self.component / "missing.rs"):
            with self.subTest(source=source):
                self.write_report(self.component, source)
                with self.assertRaises(ValueError):
                    normalize_report(self.report, self.component)

    def test_consumer_rejects_unnormalized_report(self):
        self.write_report(self.component, self.source)
        with self.assertRaises(ValueError):
            validate_report(self.report, self.component)


@unittest.skipUnless(Path(DIFF_COVER).is_file(), "diff-cover is required")
class CoverageGate(unittest.TestCase):
    def setUp(self):
        self.temp = TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.component = self.repo / "src/agent-sec-core"
        self.component.mkdir(parents=True)
        self.source = self.component / "example.rs"
        self.source.write_text("// baseline\n")
        self.git("init", "-q")
        self.git("config", "user.name", "CI fixture")
        self.git("config", "user.email", "ci@example.invalid")
        self.git("add", ".")
        self.git("commit", "-qm", "baseline")
        self.git("branch", "baseline")
        self.source.write_text("// baseline\n" + "// changed\n" * 5)
        self.git("add", ".")
        self.git("commit", "-qm", "changed lines")

    def git(self, *args):
        subprocess.run(
            [
                "git",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                *args,
            ],
            cwd=self.repo,
            check=True,
            capture_output=True,
        )

    def report(self, name, covered):
        report = self.component / name
        root = ET.Element("coverage", {"lines-valid": "5"})
        ET.SubElement(ET.SubElement(root, "sources"), "source").text = "."
        item = ET.SubElement(root, "class", {"filename": "example.rs"})
        lines = ET.SubElement(item, "lines")
        for n in range(2, 7):
            ET.SubElement(
                lines, "line", {"number": str(n), "hits": str(int(n in covered))}
            )
        ET.ElementTree(root).write(report)
        return str(report)

    def gate(self, *reports):
        return subprocess.run(
            [DIFF_COVER, *reports, "--compare-branch=baseline", "--fail-under=80"],
            cwd=self.component,
            capture_output=True,
            text=True,
        )

    def test_threshold_and_union_are_preserved(self):
        first = self.report("unit.xml", {2, 3, 4})
        self.assertNotEqual(self.gate(first).returncode, 0)
        self.report("unit.xml", {2, 3, 4, 5})
        result = self.gate(first)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("80%", result.stdout)
        self.report("unit.xml", {2, 3, 4})
        second = self.report("e2e.xml", {5, 6})
        result = self.gate(first, second)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("100%", result.stdout)


if __name__ == "__main__":
    unittest.main()
