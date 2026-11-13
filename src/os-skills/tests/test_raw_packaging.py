"""Regression checks for the component-owned raw OS Skills archive."""

import hashlib
import json
import os
import subprocess
import tarfile
import tempfile
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = ROOT / "packaging/raw/package.py"


class RawPackagingTests(unittest.TestCase):
    def build(
        self,
        source: Path,
        output: Path,
        arch: str = "x86_64",
        version: str | None = None,
    ):
        if version is None:
            version = tomllib.loads((source / "component.toml").read_text())[
                "component"
            ]["version"]
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

    def assert_payload_and_sbom(
        self, archive: Path, expected_payload: dict[str, bytes], version: str, arch: str
    ) -> None:
        manifest_path = ".anolisa/component.toml"
        expected_names = [*expected_payload, manifest_path]
        with tarfile.open(archive) as package:
            members = [member for member in package.getmembers() if member.isfile()]
            names = [member.name.removeprefix("./") for member in members]
            # Check multiplicity before a dictionary could hide duplicate members.
            self.assertCountEqual(
                names, expected_names, "archive regular-file inventory"
            )
            contents = {
                name: package.extractfile(member).read()
                for name, member in zip(names, members)
            }
        for name, expected in expected_payload.items():
            self.assertEqual(contents[name], expected, f"payload bytes: {name}")

        contract = tomllib.loads(contents[manifest_path].decode())
        self.assertEqual(contract["component"]["name"], "os-skills")
        self.assertEqual(contract["component"]["version"], version)
        self.assertIn("linux", contract["component"]["platform"]["os"])
        self.assertIn(arch, contract["component"]["platform"]["arch"])
        self.assertCountEqual(contract["install"]["modes"], ["user", "system"])
        destinations = {
            (entry["source"], entry["target"])
            for entry in contract["component"]["layout"]["files"]
        }
        self.assertIn(
            (manifest_path, "{datadir}/components/os-skills/component.toml"),
            destinations,
        )
        self.assertIn(("share/anolisa/skills/", "{datadir}/skills"), destinations)

        sbom = json.loads(archive.with_name(f"{archive.name}.cdx.json").read_text())
        records = sbom["components"]
        self.assertCountEqual(
            [record["name"] for record in records],
            expected_names,
            "SBOM file inventory",
        )
        references = [record["bom-ref"] for record in records]
        self.assertEqual(len(references), len(set(references)), "SBOM references")
        for record in records:
            self.assertEqual(record["type"], "file")
            hashes = [
                entry["content"]
                for entry in record["hashes"]
                if entry["alg"] == "SHA-256"
            ]
            expected_hash = hashlib.sha256(contents[record["name"]]).hexdigest()
            self.assertEqual(hashes, [expected_hash], f"SBOM SHA-256: {record['name']}")

    def test_fixture_payload_and_sbom_fidelity(self) -> None:
        fixture = {
            "ai/example/SKILL.md": b"# Example skill\nFixed packaging fixture.\n",
            "ai/example/references/notes.txt": b"first line\r\nsecond line\n",
            "ai/example/assets/sample.bin": b"\x00\xff\x80\x01binary\x00\r\n",
            "others/second/SKILL.md": b"# Second skill\nDistinct fixture bytes.\n",
        }
        expected_payload = {
            "share/anolisa/skills/example/SKILL.md": fixture["ai/example/SKILL.md"],
            "share/anolisa/skills/example/references/notes.txt": fixture[
                "ai/example/references/notes.txt"
            ],
            "share/anolisa/skills/example/assets/sample.bin": fixture[
                "ai/example/assets/sample.bin"
            ],
            "share/anolisa/skills/second/SKILL.md": fixture["others/second/SKILL.md"],
        }
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            contract = (ROOT / "component.toml").read_bytes()
            (source / "component.toml").write_bytes(contract)
            version = tomllib.loads(contract.decode())["component"]["version"]
            for name, data in fixture.items():
                path = source / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            subprocess.run(
                ["git", "-C", str(source), "add", "component.toml", *fixture],
                check=True,
            )
            for arch in ("x86_64", "aarch64"):
                with self.subTest(arch=arch):
                    output = root / arch
                    result = self.build(source, output, arch)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    archive = output / f"os-skills-{version}-linux-{arch}.tar.gz"
                    self.assert_payload_and_sbom(
                        archive, expected_payload, version, arch
                    )

    def test_real_payload_and_reproducibility(self):
        version = tomllib.loads((ROOT / "component.toml").read_text())["component"][
            "version"
        ]
        tracked = subprocess.check_output(
            ["git", "ls-files", "-z", "--", "."], cwd=ROOT
        )
        tracked_paths = [
            Path(name) for name in os.fsdecode(tracked).split("\0") if name
        ]
        skill_dirs = {path.parent for path in tracked_paths if path.name == "SKILL.md"}
        expected = {
            f"share/anolisa/skills/{skill.name}/{path.relative_to(skill)}"
            for skill in skill_dirs
            for path in tracked_paths
            if path.is_relative_to(skill)
        }
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary)
            for arch in ("x86_64", "aarch64"):
                self.assertEqual(self.build(ROOT, out, arch).returncode, 0)
                archive = out / f"os-skills-{version}-linux-{arch}.tar.gz"
                digest = hashlib.sha256(archive.read_bytes()).hexdigest()
                self.assertEqual(self.build(ROOT, out, arch).returncode, 0)
                self.assertEqual(
                    hashlib.sha256(archive.read_bytes()).hexdigest(), digest
                )
                with tarfile.open(archive) as package:
                    members = {
                        m.name.removeprefix("./"): m for m in package.getmembers()
                    }
                    files = {name: m for name, m in members.items() if m.isfile()}
                    self.assertEqual(files[".anolisa/component.toml"].mode, 0o644)
                    contract = tomllib.loads(
                        package.extractfile(files[".anolisa/component.toml"])
                        .read()
                        .decode()
                    )
                    self.assertEqual(contract["component"]["version"], version)
                    self.assertEqual(
                        contract["component"]["platform"]["arch"], ["x86_64", "aarch64"]
                    )
                    self.assertEqual(contract["install"]["modes"], ["user", "system"])
                    self.assertEqual(set(files), expected | {".anolisa/component.toml"})
                    self.assertTrue(
                        all(
                            m.uid == m.gid == 0 and m.mtime == 1234567890
                            for m in members.values()
                        )
                    )
                    self.assertTrue(
                        all(m.mode == 0o755 for m in members.values() if m.isdir())
                    )
                    for name, member in files.items():
                        self.assertEqual(
                            member.mode,
                            0o755 if name.endswith((".sh", ".py")) else 0o644,
                        )
                sbom = json.loads((out / f"{archive.name}.cdx.json").read_text())
                inventory = {
                    c["name"]: c["hashes"][0]["content"] for c in sbom["components"]
                }
                self.assertEqual(set(inventory), set(files))
                self.assertEqual(
                    sbom["metadata"]["component"]["hashes"][0]["content"], digest
                )
                self.assertEqual(
                    sbom["metadata"]["component"]["properties"][2]["value"], arch
                )

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
                [
                    "git",
                    "-C",
                    str(root),
                    "add",
                    "component.toml",
                    "ai/example/SKILL.md",
                ],
                check=True,
            )
            (skill / "cache.bin").write_bytes(b"untracked build output")
            output = root / "artifacts"
            self.assertNotEqual(self.build(root, output, version="9.9.9").returncode, 0)
            result = self.build(root, output)
            self.assertEqual(result.returncode, 0, result.stderr)
            with tarfile.open(
                output / f"os-skills-{fixture_version}-linux-x86_64.tar.gz"
            ) as package:
                self.assertNotIn(
                    "./share/anolisa/skills/example/cache.bin", package.getnames()
                )
            (skill / "SKILL.md").unlink()
            self.assertNotEqual(self.build(root, root / "bad").returncode, 0)


if __name__ == "__main__":
    unittest.main()
