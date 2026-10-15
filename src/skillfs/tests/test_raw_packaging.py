"""Exercise SkillFS raw package identity, payload, and reproducibility."""

from __future__ import annotations

import hashlib
import os
import shutil
import struct
import subprocess
import tarfile
import tempfile
import tomllib
import unittest
from pathlib import Path

COMPONENT = Path(__file__).resolve().parents[1]
CONTRACT = COMPONENT / ".anolisa/component.toml"
PACKER = COMPONENT / "packaging/raw/package.sh"


class SkillfsPackagingTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="skillfs-packaging-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "source"
        shutil.copytree(COMPONENT / "docs/skills", self.source / "docs/skills")
        for name in ("Cargo.toml", "component.toml", "LICENSE", "README.md"):
            shutil.copyfile(COMPONENT / name, self.source / name)
        self.contract = CONTRACT
        self.manifest = tomllib.loads(CONTRACT.read_text())
        self.version = self.manifest["component"]["version"]
        self.binary = self.source / "target/release/skillfs"
        self.binary.parent.mkdir(parents=True)
        self.metadata = self.binary.parent / "skillfs-build.toml"
        self.prepare_binary("x86_64")

    def prepare_binary(self, arch: str) -> None:
        self.arch = arch
        blob = bytearray(64)
        blob[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<H", blob, 18, {"x86_64": 62, "aarch64": 183}[arch])
        self.binary.write_bytes(blob)
        self.binary.chmod(0o755)
        self.metadata.write_text(
            f'version = "{self.version}"\ntarget_os = "linux"\ntarget_arch = "{arch}"\n'
            f'[binaries]\nskillfs = "{hashlib.sha256(blob).hexdigest()}"\n'
        )

    def pack(self, output: str = "output", **overrides: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(PACKER), "package"],
            env={
                **os.environ,
                "SKILLFS_SOURCE_DIR": str(self.source),
                "RAW_CONTRACT": str(self.contract),
                "BIN_DIR": str(self.binary.parent),
                "TARGET_OS": "linux",
                "TARGET_ARCH": self.arch,
                "OUTPUT_DIR": str(self.root / output),
                "SOURCE_DATE_EPOCH": "1700000000",
                "SKILLFS_BUILD_METADATA": str(self.metadata),
                **overrides,
            },
            capture_output=True,
            text=True,
        )

    def artifact(self, output: str = "output") -> Path:
        return self.root / output / f"skillfs-{self.version}-linux-{self.arch}.tar.gz"

    def test_payload_and_permissions_on_both_architectures(self) -> None:
        distribution = (
            COMPONENT.parents[1]
            / "distribution/anolisa/manifests/components/skillfs/component.toml"
        )
        self.assertEqual(distribution.read_bytes(), CONTRACT.read_bytes())
        for arch in ("x86_64", "aarch64"):
            with self.subTest(arch=arch):
                self.prepare_binary(arch)
                result = self.pack()
                self.assertEqual(result.returncode, 0, result.stderr)
                with tarfile.open(self.artifact()) as archive:
                    members = {member.name.removeprefix("./"): member for member in archive}
                    for entry in self.manifest["component"]["layout"]["files"]:
                        self.assertIn(entry["source"].rstrip("/"), members)
                    self.assertEqual(members["bin/skillfs"].mode, 0o755)
                    self.assertEqual(members[".anolisa/component.toml"].mode, 0o644)
                    self.assertEqual(
                        archive.extractfile("./.anolisa/component.toml").read(),
                        CONTRACT.read_bytes(),
                    )
                    self.assertEqual(
                        archive.extractfile("./share/doc/skillfs/LICENSE").read(),
                        (COMPONENT / "LICENSE").read_bytes(),
                    )
                    self.assertIn("share/anolisa/runtime/skills/skillfs/SKILL.md", members)
                    self.assertTrue(all(not member.issym() for member in members.values()))

    def test_reproducible_archive(self) -> None:
        first = self.pack("first")
        self.assertEqual(first.returncode, 0, first.stderr)
        for path in self.source.rglob("*"):
            os.utime(path, (1800000000, 1800000000))
        second = self.pack("second")
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(self.artifact("first").read_bytes(), self.artifact("second").read_bytes())

    def test_contract_version_mismatch(self) -> None:
        self.contract = self.root / "wrong-contract.toml"
        self.contract.write_text(CONTRACT.read_text().replace(self.version, "99.0.0"))
        result = self.pack()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("contract identity", result.stderr)

    def test_rpm_version_mismatch(self) -> None:
        rpm = self.source / "component.toml"
        rpm.write_text(rpm.read_text().replace(self.version, "99.0.0"))
        result = self.pack()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("RPM component identity", result.stderr)

    def test_missing_skill(self) -> None:
        (self.source / "docs/skills/skillfs-mount/SKILL.md").unlink()
        result = self.pack()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing bundled", result.stderr)

    def test_wrong_elf_architecture(self) -> None:
        result = self.pack(TARGET_ARCH="aarch64")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ELF architecture", result.stderr)

    def test_binary_hash_mismatch(self) -> None:
        self.binary.write_bytes(self.binary.read_bytes() + b"changed")
        result = self.pack()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("SHA-256", result.stderr)

    def test_metadata_identity_mismatch(self) -> None:
        original = self.metadata.read_text()
        for old, new in ((self.version, "99.0.0"), ("linux", "macos"), ("x86_64", "aarch64")):
            with self.subTest(field=old):
                self.metadata.write_text(original.replace(old, new))
                result = self.pack()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("build metadata", result.stderr)

    def test_cross_binary_requires_metadata(self) -> None:
        self.prepare_binary("aarch64" if os.uname().machine == "x86_64" else "x86_64")
        self.metadata.unlink()
        result = self.pack(SKILLFS_BUILD_METADATA="")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cross-target packaging requires build metadata", result.stderr)

    def test_unsupported_os(self) -> None:
        result = self.pack(TARGET_OS="macos")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported skillfs target", result.stderr)


if __name__ == "__main__":
    unittest.main()
