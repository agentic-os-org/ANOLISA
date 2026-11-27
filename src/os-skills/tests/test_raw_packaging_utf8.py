"""Regression tests for #10783: UTF-8 skill paths must not acquire surrogate escapes.

Under the C locale, os.fsdecode() decodes git ls-files -z output with
surrogateescape, so a UTF-8 filename becomes lone surrogates. tarfile encodes
those surrogates back to the real bytes but json.dumps() keeps the escapes, so
the SBOM name stops matching the tar member. The packager must decode git's
raw bytes as UTF-8 explicitly, independent of the process locale.
"""

import json
import os
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = ROOT / "packaging/raw/package.py"


class Utf8PathPackagingTests(unittest.TestCase):
    def _load_packager(self):
        import importlib.util

        spec = importlib.util.spec_from_file_location(
            "raw_package_10783", ROOT / "packaging/raw/package.py"
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def test_git_utf8_path_bytes_decode_without_surrogates(self):
        """The decode boundary must yield real Unicode, not lone surrogates.

        This is the exact transformation that broke under C: git emits the
        raw UTF-8 bytes for 'café.txt' (c3 a9); os.fsdecode under C locale
        produces '\\udcc3\\udca9'. Explicit UTF-8 decoding yields 'café.txt'.
        """
        module = self._load_packager()
        raw = b"ai/qwenpaw-usage/caf\xc3\xa9.txt"
        decoded = module.decode_git_path(raw)
        self.assertEqual(decoded, "ai/qwenpaw-usage/caf\u00e9.txt")
        self.assertFalse(any(0xDC80 <= ord(c) <= 0xDCFF for c in decoded), ascii(decoded))

    def test_utf8_filename_matches_between_tar_and_sbom(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "src"
            skill = source / "ai" / "qwenpaw-usage"
            (source / "packaging").mkdir(parents=True)
            skill.mkdir(parents=True)
            (source / "component.toml").write_text((ROOT / "component.toml").read_text())
            (source / "packaging" / "released-skills.txt").write_text("ai/qwenpaw-usage\n")
            (skill / "SKILL.md").write_text("# Fixture\n")
            utf8_name = "caf\u00e9.txt"
            (skill / utf8_name).write_text("payload\n")
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            subprocess.run(["git", "-C", str(source), "add", "."], check=True)

            version = tomllib.loads((ROOT / "component.toml").read_text())["component"]["version"]
            module = self._load_packager()
            output = root / "out"
            module.package(source, output, version, "x86_64", 0)

            archive = output / f"os-skills-{version}-linux-x86_64.tar.gz"
            sbom = json.loads(
                (output / f"{archive.name}.cdx.json").read_text(encoding="utf-8")
            )
            with tarfile.open(archive) as package:
                tar_names = {
                    m.name.removeprefix("./") for m in package.getmembers() if m.isfile()
                }
            sbom_names = {c["name"] for c in sbom["components"]}
            expected = "share/anolisa/skills/qwenpaw-usage/" + utf8_name
            self.assertIn(expected, tar_names, ascii(sorted(tar_names)))
            self.assertIn(expected, sbom_names, ascii(sorted(sbom_names)))
            self.assertEqual(tar_names, sbom_names, "tar and SBOM inventories must agree")

    def test_packager_succeeds_under_c_locale_via_subprocess(self):
        """Run the packager itself under the C locale, matching #10783.

        A direct call to package() cannot exercise this: the constructor reads
        sys.flags.utf8_mode of the *test* interpreter, which CI starts as UTF-8.
        Only a fresh process with LC_ALL=C PYTHONUTF8=0 reproduces the reported
        ASCII filesystem encoding, where decoded Unicode paths must still be
        stat()-able. The packager re-execs itself into UTF-8 mode at entry.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "src"
            skill = source / "ai" / "qwenpaw-usage"
            (source / "packaging").mkdir(parents=True)
            skill.mkdir(parents=True)
            version = tomllib.loads((ROOT / "component.toml").read_text())["component"]["version"]
            (source / "component.toml").write_text((ROOT / "component.toml").read_text())
            (source / "packaging" / "released-skills.txt").write_text("ai/qwenpaw-usage\n")
            (skill / "SKILL.md").write_text("# Fixture\n")
            (skill / "caf\u00e9.txt").write_text("payload\n")
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            subprocess.run(["git", "-C", str(source), "add", "."], check=True)

            for locale in ("C", "C.UTF-8"):
                output = root / locale
                env = dict(os.environ)
                env.update({"LC_ALL": locale, "PYTHONUTF8": "0", "PYTHONCOERCECLOCALE": "0"})
                result = subprocess.run(
                    [
                        sys.executable,
                        str(PACKAGE),
                        "--source-dir",
                        str(source),
                        "--output-dir",
                        str(output),
                        "--version",
                        version,
                        "--target-arch",
                        "x86_64",
                        "--epoch",
                        "0",
                    ],
                    capture_output=True,
                    text=True,
                    env=env,
                )
                self.assertEqual(result.returncode, 0, f"{locale}: {result.stderr}")
                archive = output / f"os-skills-{version}-linux-x86_64.tar.gz"
                self.assertTrue(archive.is_file(), f"{locale}: no archive produced")
                sbom = json.loads(
                    (output / f"{archive.name}.cdx.json").read_text(encoding="utf-8")
                )
                with tarfile.open(archive) as package:
                    tar_names = {
                        m.name.removeprefix("./") for m in package.getmembers() if m.isfile()
                    }
                sbom_names = {c["name"] for c in sbom["components"]}
                self.assertEqual(tar_names, sbom_names, f"{locale}: inventories disagree")
                self.assertTrue(
                    all(not any(0xDC80 <= ord(c) <= 0xDCFF for c in n) for n in sbom_names),
                    f"{locale}: surrogate escapes in SBOM names",
                )


if __name__ == "__main__":
    unittest.main()
