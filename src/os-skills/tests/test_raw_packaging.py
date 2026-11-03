"""Regression checks for the component-owned raw OS Skills archive."""

import hashlib
import json
import subprocess
import tarfile
import tempfile
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = ROOT / "packaging/raw/package.py"


class RawPackagingTests(unittest.TestCase):
    def build(self, source: Path, output: Path, arch: str = "x86_64", version: str | None = None):
        if version is None:
            version = tomllib.loads((source / "component.toml").read_text())["component"]["version"]
        return subprocess.run(
            [
                "python3",
                str(PACKAGE),
                "--source-dir",
                str(source),
                "--output-dir",
                str(output),
                "--version",
                version,
                "--target-arch",
                arch,
                "--epoch",
                "1234567890",
            ],
            capture_output=True,
            text=True,
        )

    def test_real_payload_and_reproducibility(self):
        version = tomllib.loads((ROOT / "component.toml").read_text())["component"]["version"]
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary)
            for arch in ("x86_64", "aarch64"):
                self.assertEqual(self.build(ROOT, out, arch).returncode, 0)
                archive = out / f"os-skills-{version}-linux-{arch}.tar.gz"
                digest = hashlib.sha256(archive.read_bytes()).hexdigest()
                self.assertEqual(self.build(ROOT, out, arch).returncode, 0)
                self.assertEqual(hashlib.sha256(archive.read_bytes()).hexdigest(), digest)
                with tarfile.open(archive) as package:
                    members = {m.name.removeprefix("./"): m for m in package.getmembers()}
                    files = {name: m for name, m in members.items() if m.isfile()}
                    self.assertEqual(files[".anolisa/component.toml"].mode, 0o644)
                    contract = tomllib.loads(
                        package.extractfile(files[".anolisa/component.toml"]).read().decode()
                    )
                    self.assertEqual(contract["component"]["version"], version)
                    self.assertEqual(
                        contract["component"]["platform"]["arch"], ["x86_64", "aarch64"]
                    )
                    self.assertEqual(contract["install"]["modes"], ["user", "system"])
                    expected = {
                        f"share/anolisa/skills/{skill.parent.name}/{file.relative_to(skill.parent)}"
                        for skill in ROOT.rglob("SKILL.md")
                        for file in skill.parent.rglob("*")
                        if file.is_file()
                    }
                    self.assertEqual(set(files), expected | {".anolisa/component.toml"})
                    self.assertTrue(
                        all(m.uid == m.gid == 0 and m.mtime == 1234567890 for m in members.values())
                    )
                    self.assertTrue(all(m.mode == 0o755 for m in members.values() if m.isdir()))
                    for name, member in files.items():
                        self.assertEqual(
                            member.mode, 0o755 if name.endswith((".sh", ".py")) else 0o644
                        )
                sbom = json.loads((out / f"{archive.name}.cdx.json").read_text())
                inventory = {c["name"]: c["hashes"][0]["content"] for c in sbom["components"]}
                self.assertEqual(set(inventory), set(files))
                self.assertEqual(sbom["metadata"]["component"]["hashes"][0]["content"], digest)
                self.assertEqual(sbom["metadata"]["component"]["properties"][2]["value"], arch)

    def test_rejects_wrong_version_and_ignores_untracked_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_contract = (ROOT / "component.toml").read_text()
            source_version = tomllib.loads(source_contract)["component"]["version"]
            prefix, patch = source_version.rsplit(".", 1)
            fixture_version = f"{prefix}.{int(patch) + 1}"
            (root / "component.toml").write_text(
                source_contract.replace(
                    f'\nversion = "{source_version}"\n',
                    f'\nversion = "{fixture_version}"\n',
                    1,
                )
            )
            skill = root / "ai/example"
            skill.mkdir(parents=True)
            (skill / "SKILL.md").write_text("tracked")
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            subprocess.run(
                ["git", "-C", str(root), "add", "component.toml", "ai/example/SKILL.md"], check=True
            )
            (skill / "cache.bin").write_bytes(b"untracked build output")
            output = root / "artifacts"
            self.assertNotEqual(self.build(root, output, version="9.9.9").returncode, 0)
            result = self.build(root, output)
            self.assertEqual(result.returncode, 0, result.stderr)
            with tarfile.open(output / f"os-skills-{fixture_version}-linux-x86_64.tar.gz") as package:
                self.assertNotIn("./share/anolisa/skills/example/cache.bin", package.getnames())
            (skill / "SKILL.md").unlink()
            self.assertNotEqual(self.build(root, root / "bad").returncode, 0)


if __name__ == "__main__":
    unittest.main()
