"""Exercise the release-catalog CLI used by installation and adapter checks."""

import ast
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "packaging/release_catalog.py"


class ReleaseCatalogCliTest(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.source = Path(temporary.name)
        (self.source / "packaging").mkdir()
        (self.source / "packaging/released-skills.txt").write_text(
            "# default delivery\ndevops/released\n", encoding="utf-8"
        )
        for name in ("released", "incubating"):
            skill = self.source / "devops" / name
            skill.mkdir(parents=True)
            (skill / "SKILL.md").write_text(name + "\n", encoding="utf-8")
        (self.source / "component.toml").write_text(
            '[component]\nname = "os-skills"\n'
            '[[adapters]]\nframework = "hermes"\n'
            '[[adapters.hermes.skills]]\nname = "released"\n'
            'source = "{datadir}/skills/released/"\n'
            '[[adapters.hermes.skills]]\nname = "incubating"\n'
            'source = "{datadir}/skills/incubating/"\n',
            encoding="utf-8",
        )

    def run_catalog(self, *arguments: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(SCRIPT), *arguments],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            universal_newlines=True,
        )

    def test_paths_output_is_relative_skill_manifests_in_catalog_order(self) -> None:
        result = self.run_catalog("paths", str(self.source))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "./devops/released/SKILL.md\n")

    def test_manifest_and_staging_exclude_incubating_skill(self) -> None:
        output = self.source / "filtered.toml"
        result = self.run_catalog("manifest", str(self.source), str(output))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('name = "released"', output.read_text(encoding="utf-8"))
        self.assertNotIn("incubating", output.read_text(encoding="utf-8"))
        for layout, manifest in (
            ("source", "devops/released/SKILL.md"),
            ("flat", "released/SKILL.md"),
        ):
            destination = self.source / layout
            result = self.run_catalog(
                "stage", str(self.source), str(destination), "--layout", layout
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                list(destination.rglob("SKILL.md")), [destination / manifest]
            )

    def test_stage_excludes_python_bytecode_caches(self) -> None:
        scripts = self.source / "devops/released/scripts"
        pycache = scripts / "__pycache__"
        pycache.mkdir(parents=True)
        (scripts / "helper.py").write_text("print('ok')\n", encoding="utf-8")
        (pycache / "helper.cpython-311.pyc").write_bytes(b"compiled")
        (scripts / "legacy.pyc").write_bytes(b"compiled-pyc")
        (scripts / "legacy.pyo").write_bytes(b"compiled-pyo")

        for layout, prefix in (("source", "devops/released"), ("flat", "released")):
            destination = self.source / ("staged-" + layout)
            result = self.run_catalog(
                "stage", str(self.source), str(destination), "--layout", layout
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            staged_files = {
                path.relative_to(destination).as_posix()
                for path in destination.rglob("*")
            }
            self.assertIn(prefix + "/scripts/helper.py", staged_files)
            self.assertFalse(
                any(
                    "__pycache__" in item or item.endswith((".pyc", ".pyo"))
                    for item in staged_files
                ),
                f"Python bytecode caches leaked into {layout} stage: {staged_files}",
            )

        (scripts / "linked.pyc").symlink_to(scripts / "helper.py")
        bad = self.run_catalog(
            "stage", str(self.source), str(self.source / "bad"), "--layout", "flat"
        )
        self.assertNotEqual(bad.returncode, 0)
        self.assertIn("symlink", bad.stderr)

    def test_missing_action_reports_usage(self) -> None:
        result = self.run_catalog()
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage:", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    @unittest.skipIf(
        sys.version_info < (3, 8),
        "Older interpreters parse these tools when running them",
    )
    def test_catalog_tools_keep_python36_syntax(self) -> None:
        for name in ("release_catalog.py", "cosh_manifests.py"):
            source = (ROOT / "packaging" / name).read_text(encoding="utf-8")
            ast.parse(source, filename=name, feature_version=(3, 6))


if __name__ == "__main__":
    unittest.main()
