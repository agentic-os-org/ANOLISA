#!/usr/bin/env python3
"""Check source layout independently from release archive identity."""

import os
import re
import subprocess
import tarfile
import tempfile
import textwrap
import unittest
from pathlib import Path


HERE = Path(__file__).resolve().parent
ACTION = (HERE.parent / "actions/package-source/action.yaml").read_text(encoding="utf-8")


def run_block(name: str) -> str:
    step = ACTION.split(f"    - name: {name}\n", 1)[1].split("    - name:", 1)[0]
    match = re.search(r"      run: \|\n((?:        [^\n]*\n|\n)+)", step)
    if match is None:
        raise AssertionError(f"Missing Bash block: {name}")
    return textwrap.dedent(match.group(1))


class PackageSourcePathsTest(unittest.TestCase):
    def test_source_paths_preserve_archive_identity(self) -> None:
        for component, source in (
            ("anolisa", "distribution/anolisa"),
            ("copilot-shell", "deprecated/copilot-shell"),
            ("cosh-ng", "src/cosh-ng"),
        ):
            for suffix in ("", ".preview"):
                with self.subTest(component=component, suffix=suffix):
                    self.check_archive(component, source, suffix)

    def check_archive(self, component: str, source: str, suffix: str) -> None:
        with tempfile.TemporaryDirectory(dir=HERE, prefix="package-source-") as temporary:
            root = Path(temporary)
            source_dir = root / source
            source_dir.mkdir(parents=True)
            (source_dir / "payload.txt").write_text(component, encoding="utf-8")
            (root / "LICENSE").write_text("fixture license\n", encoding="utf-8")
            (source_dir / "LICENSE").symlink_to("../../LICENSE")
            env_file = root / "github-env"
            env_file.touch()
            env = dict(os.environ, GITHUB_ENV=str(env_file), TOKENLESS_SOURCE_ROOT=".")
            inputs = {
                "component": component,
                "version": "1.2.3",
                "artifact-suffix": suffix,
            }
            for name in (
                "Resolve source directory",
                "Resolve LICENSE",
                "Create source archive",
                "Create source archive (tar)",
            ):
                script = run_block(name)
                for key, value in inputs.items():
                    script = script.replace("${{ inputs." + key + " }}", value)
                # Keep the action's copy/tar commands but isolate their scratch paths.
                script = script.replace("/tmp/build", str(root / "build"))
                script = script.replace("/tmp/archives", str(root / "archives"))
                result = subprocess.run(
                    ["bash", "-euo", "pipefail", "-c", script],
                    cwd=root,
                    env=env,
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                env.update(line.split("=", 1) for line in env_file.read_text().splitlines())

            archive_name = f"{component}-1.2.3"
            archive_file = f"{archive_name}{suffix}.tar.gz"
            self.assertEqual(env["SRC_DIR"], source)
            self.assertEqual(env["ARCHIVE_NAME"], archive_name)
            self.assertEqual(env["ARCHIVE_FILE"], archive_file)
            with tarfile.open(root / "archives" / archive_file, "r:gz") as archive:
                self.assertEqual(
                    {name.split("/", 1)[0] for name in archive.getnames()}, {archive_name}
                )
                payload = archive.extractfile(f"{archive_name}/payload.txt")
                self.assertIsNotNone(payload)
                self.assertEqual(payload.read().decode(), component)
                license_member = archive.getmember(f"{archive_name}/LICENSE")
                self.assertTrue(license_member.isfile())
                self.assertTrue((source_dir / "LICENSE").is_symlink())


if __name__ == "__main__":
    unittest.main()
